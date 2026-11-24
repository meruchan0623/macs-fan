//! Process boundary for driver calls. No hardware calls run in the UI process.
use crate::monitor::{Monitor, Snapshot};
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(6);
pub(crate) const MAX_LINE: usize = 4 * 1024 * 1024;
const MAX_REQUEST: usize = 20;

#[derive(serde::Serialize, serde::Deserialize)]
struct WireResponse {
    request_id: u64,
    snapshot: Snapshot,
}

type Response = Result<Snapshot, String>;

/// One cached Monitor lives only in the child. The stdin watcher terminates the
/// whole worker on EOF, including when its main thread is inside a driver call.
/// SIGKILL/exit still cannot promise termination of uninterruptible kernel work.
pub fn run_worker() -> Result<(), String> {
    let (requests, incoming) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("sampler-parent-watch".into())
        .spawn(move || {
            let mut input = BufReader::new(io::stdin());
            let mut previous = 0;
            loop {
                match bounded_line(&mut input, MAX_REQUEST) {
                    Ok(Some(line)) => {
                        let id = std::str::from_utf8(&line)
                            .ok()
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(0);
                        if id <= previous {
                            std::process::exit(2);
                        }
                        previous = id;
                        if requests.try_send(id).is_err() {
                            std::process::exit(2);
                        }
                    }
                    Ok(None) => std::process::exit(0),
                    _ => std::process::exit(2),
                }
            }
        })
        .map_err(|e| format!("无法启动父进程监测：{e}"))?;
    let mut monitor = Monitor::default();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    while let Ok(request_id) = incoming.recv() {
        let bytes = serde_json::to_vec(&WireResponse {
            request_id,
            snapshot: monitor.snapshot(),
        })
        .map_err(|e| format!("采样序列化失败：{e}"))?;
        if bytes.len() > MAX_LINE {
            return Err("采样响应超过4MiB".into());
        }
        output
            .write_all(&bytes)
            .and_then(|_| output.write_all(b"\n"))
            .and_then(|_| output.flush())
            .map_err(|e| format!("采样响应写入失败：{e}"))?;
    }
    Ok(())
}

// Read at most limit + one delimiter. read_until would allow an unbounded allocation.
fn bounded_line(reader: &mut impl BufRead, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|e| format!("采样管道读取失败：{e}"))?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err("采样响应缺少换行".into())
            };
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let count = newline.unwrap_or(available.len());
        if count > limit.saturating_sub(line.len()) {
            return Err("采样协议行超过长度上限".into());
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

struct Worker {
    child: Child,
    requests: Option<SyncSender<u64>>,
    responses: Receiver<Result<WireResponse, String>>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

impl Worker {
    fn spawn(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("无法启动采样子进程：{e}"))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stdin = child.stdin.take().expect("piped stdin");
        let (responses_tx, responses) = mpsc::sync_channel(1);
        let writer_errors = responses_tx.clone();
        let (requests, incoming) = mpsc::sync_channel(1);
        // Spawn failure must also retain ownership of the child for cleanup.
        let reader = match thread::Builder::new()
            .name("sampler-stdout".into())
            .spawn(move || {
                let mut input = BufReader::new(stdout);
                loop {
                    let response = match bounded_line(&mut input, MAX_LINE) {
                        Ok(Some(bytes)) => {
                            serde_json::from_slice(&bytes).map_err(|e| format!("采样JSON无效：{e}"))
                        }
                        Ok(None) => Err("采样子进程已关闭输出".into()),
                        Err(e) => Err(e),
                    };
                    let failed = response.is_err();
                    if responses_tx.try_send(response).is_err() || failed {
                        break;
                    }
                }
            }) {
            Ok(handle) => handle,
            Err(error) => {
                // No reader thread exists; transfer the child to a reaper.
                thread::spawn(move || {
                    let _ = child.kill();
                    reap(&mut child);
                });
                return Err(format!("无法启动采样读取线程：{error}"));
            }
        };
        let writer = match thread::Builder::new()
            .name("sampler-stdin".into())
            .spawn(move || {
                while let Ok(id) = incoming.recv() {
                    if let Err(e) = writeln!(stdin, "{id}").and_then(|_| stdin.flush()) {
                        let _ = writer_errors.try_send(Err(format!("采样请求写入失败：{e}")));
                        break;
                    }
                }
            }) {
            Ok(handle) => handle,
            Err(error) => {
                drop(requests);
                thread::spawn(move || {
                    let _ = child.kill();
                    reap(&mut child);
                    let _ = reader.join();
                });
                return Err(format!("无法启动采样写入线程：{error}"));
            }
        };
        Ok(Self {
            child,
            requests: Some(requests),
            responses,
            reader: Some(reader),
            writer: Some(writer),
        })
    }

    fn cleanup(mut self) -> Result<(), String> {
        self.requests.take();
        // wait is deliberately outside the request/UI thread. If the OS cannot
        // reap this child, Manager stays closed instead of accumulating workers.
        let _ = self.child.kill();
        reap(&mut self.child);
        if let Some(handle) = self.reader.take() {
            handle
                .join()
                .map_err(|_| "采样读取线程退出失败".to_string())?;
        }
        if let Some(handle) = self.writer.take() {
            handle
                .join()
                .map_err(|_| "采样写入线程退出失败".to_string())?;
        }
        Ok(())
    }
}

// Keep ownership on wait errors as well as uninterruptible waits. Never silently
// drop a possibly-live Child after an OS error. One reaper can remain pending;
// the Manager will not create a replacement until it reports success.
fn reap(child: &mut Child) {
    let mut reported = false;
    loop {
        match child.wait() {
            Ok(_) => return,
            Err(error) => {
                if !reported {
                    eprintln!("采样子进程尚未确认退出，暂停重建：{error}");
                    reported = true;
                }
                let _ = child.kill();
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

#[derive(Default)]
pub struct Manager {
    worker: Option<Worker>,
    cleanup: Option<Receiver<Result<(), String>>>,
    closed: Option<String>,
    next_request_id: u64,
}

impl Manager {
    pub fn request_snapshot(&mut self) -> Response {
        self.request_with(DEADLINE, || {
            let executable =
                std::env::current_exe().map_err(|e| format!("无法定位采样可执行文件：{e}"))?;
            let mut command = Command::new(executable);
            command.arg("--sampler-worker");
            Ok(command)
        })
    }

    fn request_with(
        &mut self,
        timeout: Duration,
        command: impl FnOnce() -> Result<Command, String>,
    ) -> Response {
        let deadline = Instant::now() + timeout;
        if let Some(error) = &self.closed {
            return Err(error.clone());
        }
        if let Some(cleanup) = &self.cleanup {
            match cleanup.try_recv() {
                Ok(Ok(())) => self.cleanup = None,
                Ok(Err(error)) => {
                    self.closed = Some(error.clone());
                    return Err(error);
                }
                Err(TryRecvError::Empty) => return Err("采样子进程仍在回收，已暂停重建".into()),
                Err(TryRecvError::Disconnected) => {
                    let error = "采样回收线程失联，已暂停重建".to_string();
                    self.closed = Some(error.clone());
                    return Err(error);
                }
            }
        }
        if self.worker.is_none() {
            match Worker::spawn(&mut command()?) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    // A setup failure can happen after spawning but before its
                    // pipe threads exist. Its reaper may still be running.
                    self.closed = Some(error.clone());
                    return Err(error);
                }
            }
        }
        if Instant::now() >= deadline {
            self.retire();
            return Err("采样启动已超过截止时间".into());
        }
        let Some(request_id) = self.next_request_id.checked_add(1) else {
            self.retire();
            self.closed = Some("采样请求序号已耗尽".into());
            return Err("采样请求序号已耗尽".into());
        };
        self.next_request_id = request_id;
        let worker = self.worker.as_mut().expect("worker started");
        let result = worker
            .requests
            .as_ref()
            .expect("active writer")
            .try_send(request_id)
            .map_err(|e| format!("采样请求队列不可用：{e}"))
            .and_then(|_| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                worker
                    .responses
                    .recv_timeout(remaining)
                    .map_err(|e| format!("采样未在截止时间内完成：{e}"))?
            });
        let result = result.and_then(|response| {
            if response.request_id != request_id {
                Err("采样响应序号不匹配，已拒绝旧响应".into())
            } else {
                Ok(response.snapshot)
            }
        });
        if result.is_err() {
            self.retire();
        }
        result
    }

    fn retire(&mut self) {
        if let Some(worker) = self.worker.take() {
            let (done, cleanup) = mpsc::sync_channel(1);
            self.cleanup = Some(cleanup);
            // The reaper owns the child, both pipe threads and wait. It is never
            // abandoned merely because the request deadline elapsed.
            thread::spawn(move || {
                let _ = done.send(worker.cleanup());
            });
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.retire();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GOOD: &str = r#"{"model":"fixture","architecture":"test","capturedAtMs":1,"status":"ready","message":"fixture","controlAvailable":false,"fans":[],"sensors":[],"diagnostics":[]}"#;

    // An executable plus literal arguments; no shell and no hardware access.
    fn fake(body: &str) -> Command {
        let mut command = Command::new("python3");
        command.args(["-u", "-c", body]);
        command
    }
    fn request(manager: &mut Manager, body: &str) -> Response {
        manager.request_with(Duration::from_secs(3), || Ok(fake(body)))
    }
    fn wait_cleanup(manager: &mut Manager) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while manager
            .cleanup
            .as_ref()
            .is_some_and(|rx| match rx.try_recv() {
                Ok(Ok(())) => false,
                Ok(Err(error)) => panic!("{error}"),
                Err(TryRecvError::Empty) => true,
                Err(TryRecvError::Disconnected) => panic!("reaper disconnected"),
            })
        {
            assert!(Instant::now() < deadline, "worker cleanup did not complete");
            thread::sleep(Duration::from_millis(5));
        }
        manager.cleanup = None;
    }

    #[test]
    fn bounded_protocol_rejects_partial_and_oversize() {
        assert_eq!(
            bounded_line(&mut &b"abc\n"[..], 3).unwrap().unwrap(),
            b"abc"
        );
        assert!(bounded_line(&mut &b"abcd\n"[..], 3).is_err());
        assert!(bounded_line(&mut &b"abc"[..], 3).is_err());
        assert!(bounded_line(&mut &b""[..], 3).unwrap().is_none());
    }

    #[test]
    fn valid_responses_reuse_a_single_worker() {
        let mut manager = Manager::default();
        let script = format!("import sys,json\nfor line in sys.stdin:\n print(json.dumps(dict(request_id=int(line), snapshot=json.loads({GOOD:?}))), flush=True)");
        assert_eq!(request(&mut manager, &script).unwrap().model, "fixture");
        let pid = manager.worker.as_ref().unwrap().child.id();
        assert_eq!(
            request(&mut manager, "raise Exception('must not spawn')")
                .unwrap()
                .model,
            "fixture"
        );
        assert_eq!(manager.worker.as_ref().unwrap().child.id(), pid);
        manager.retire();
        wait_cleanup(&mut manager);
    }

    #[test]
    fn malformed_oversize_and_eof_are_reaped_then_recover() {
        for body in [
            "import sys; sys.stdin.readline(); print('not json', flush=True)",
            "import sys; sys.stdin.readline(); print('x' * (4*1024*1024+1), flush=True)",
            "import sys; sys.stdin.readline()",
            "import sys; sys.stdin.readline(); sys.stdout.write('{}'); sys.stdout.flush()",
        ] {
            let mut manager = Manager::default();
            assert!(request(&mut manager, body).is_err());
            assert!(manager.worker.is_none());
            wait_cleanup(&mut manager);
            let script = format!("import sys,json; line=sys.stdin.readline(); print(json.dumps(dict(request_id=int(line), snapshot=json.loads({GOOD:?}))), flush=True)");
            assert!(request(&mut manager, &script).is_ok());
            manager.retire();
            wait_cleanup(&mut manager);
        }
    }

    #[test]
    fn blocked_worker_has_deadline_and_recovers_after_reaping() {
        let mut manager = Manager::default();
        let started = Instant::now();
        let response = manager.request_with(Duration::from_millis(250), || {
            Ok(fake(
                "import sys,time; sys.stdin.readline(); time.sleep(60)",
            ))
        });
        assert!(response.unwrap_err().contains("截止时间"));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(manager.worker.is_none());
        wait_cleanup(&mut manager);
        let script = format!("import sys,json; line=sys.stdin.readline(); print(json.dumps(dict(request_id=int(line), snapshot=json.loads({GOOD:?}))), flush=True)");
        assert!(request(&mut manager, &script).is_ok());
        manager.retire();
        wait_cleanup(&mut manager);
    }

    #[test]
    fn pending_or_failed_cleanup_never_spawns_another_child() {
        let (done, cleanup) = mpsc::sync_channel(1);
        let mut manager = Manager::default();
        manager.cleanup = Some(cleanup);
        let result = manager.request_with(DEADLINE, || panic!("must not spawn"));
        assert!(result.unwrap_err().contains("仍在回收"));
        done.send(Err("wait failed".into())).unwrap();
        assert_eq!(
            manager
                .request_with(DEADLINE, || panic!("must not spawn"))
                .unwrap_err(),
            "wait failed"
        );
        assert_eq!(
            manager
                .request_with(DEADLINE, || panic!("must not spawn"))
                .unwrap_err(),
            "wait failed"
        );
    }

    #[test]
    fn duplicate_response_cannot_satisfy_a_later_request() {
        let mut manager = Manager::default();
        let script = format!(
            "import sys,json,time; line=sys.stdin.readline(); reply=json.dumps(dict(request_id=int(line),snapshot=json.loads({GOOD:?}))); print(reply,flush=True); time.sleep(.05); print(reply,flush=True); time.sleep(60)"
        );
        assert!(request(&mut manager, &script).is_ok());
        thread::sleep(Duration::from_millis(150));
        let error = request(&mut manager, "raise Exception('must not spawn')").unwrap_err();
        assert!(error.contains("序号不匹配"), "{error}");
        assert!(manager.worker.is_none());
        wait_cleanup(&mut manager);
    }

    #[test]
    fn retirement_waits_for_child_and_pipe_threads() {
        let mut manager = Manager::default();
        let script = format!(
            "import sys,time,json; line=sys.stdin.readline(); print(json.dumps(dict(request_id=int(line), snapshot=json.loads({GOOD:?}))), flush=True); time.sleep(60)"
        );
        assert!(request(&mut manager, &script).is_ok());
        let worker = manager.worker.take().unwrap();
        // Observe the actual child exit through a temporary reaper completion.
        manager.worker = Some(worker);
        manager.retire();
        let completion = manager.cleanup.take().unwrap();
        drop(manager);
        assert!(completion
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn drop_reaps_the_live_child() {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        let mut manager = Manager::default();
        let script = format!(
            "import sys,time,json; line=sys.stdin.readline(); print(json.dumps(dict(request_id=int(line), snapshot=json.loads({GOOD:?}))), flush=True); time.sleep(60)"
        );
        assert!(request(&mut manager, &script).is_ok());
        let pid = manager.worker.as_ref().unwrap().child.id() as i32;
        drop(manager);
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { kill(pid, 0) } == 0 {
            assert!(Instant::now() < deadline, "Drop did not reap the child");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
