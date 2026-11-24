# Preferences / preset contract (0.6)

Independent application domain only. Saving/selecting a preset edits configuration, never applies hardware. GUI remains readonly. Original QSettings domain is never accessed. Layout stays 560×440/min460×340; edit on demand.

Tauri commands:
- `preferences_read()` -> Document
- `preferences_update({expectedRevision, update})` -> committed Document; errors reject with a concise Chinese String. Conflicts explicitly contain `配置已被其他窗口或进程修改`.

Document JSON (camelCase; schemaVersion=1):
```
{schemaVersion:1,revision:0,presets:[],editorPresetId:null,
 view:{scope:"overview",sort:"name",unit:"celsius",precision:"decimal"}}
```
Revision is an integer <= Number.MAX_SAFE_INTEGER; increments only committed changes. Maximum64 presets. Stable random backend-generated IDs, not names/list positions. `editorPresetId` is editor selection, NOT applied/active hardware state.

Preset:
```
{id:"32 lowercase hex",name:"安静",model:"Mac16,8",architecture:"arm64",
 fans:[{fanId:"F0",mode:{type:"automatic"}},
       {fanId:"F1",mode:{type:"fixed",rpm:2500}}]}
```
Sensor mode: `{type:"sensor",sensorId:"Tp01",minRpm:2317,maxRpm:7826,lowerTemp:45,upperTemp:85}`.
Portable storage validation: name trimmed nonempty <=64 Unicode characters/256 UTF8 bytes, reject control chars and case-insensitive duplicate names; model nonempty <=64 bytes ASCII alnum/comma; architecture nonempty <=32 bytes ASCII alnum/underscore; 1..16 fan entries with unique `F0`..`FF` IDs (F + one uppercase hex digit); rpm integer0..30000; sensor mode min<max, finite lowerTemp>=0, upperTemp<=127, lower<upper; sensorId nonempty <=256 bytes no controls. Reject unknown fields/types/schema, duplicate IDs, dangling editor selection, oversized file. Model/sensor/range matching at save is a UI aid; this schema NEVER certifies hardware applicability. A future control service must revalidate trusted capabilities at apply time. Profiles for other hardware remain stored and editable but clearly indicate mismatch; missing sensors must not silently be replaced.

Update tagged union (`type`):
- `savePreset`: `{type:"savePreset",preset:{id:null|existingId,name,model,architecture,fans}}`; null generates new ID, existing required otherwise; return doc with editorPresetId set to saved ID.
- `deletePreset`: `{type:"deletePreset",id}`; existing required; deleting editor selection clears it. No hardware effect.
- `selectPreset`: `{type:"selectPreset",id:null|existingId}`.
- `setView`: `{type:"setView",view:{scope,sort,unit,precision}}` full view value; scope overview/components/all, sort name/temperature, unit celsius/fahrenheit, precision whole/decimal.

Storage: derive path internally from Tauri app_config_dir; fixed filename preferences.json, no client paths. Create private app dir only on write; missing file returns defaults without writing. Existing malformed, future schema, nonregular/symlink file must error and stay untouched. Bounded reads <=1MiB, tempfile create_new 0600, sync file -> atomic rename -> sync parent. Cross-process lock + reread under lock + expected revision check. Reject symlinked dedicated application directory/target/lock; no privilege changes or trust edits. Avoid claiming successful durability if sync fails. Tests use isolated temp directories, never real prefs or hardware.

Frontend: existing curve footer becomes 预设. Compact modal provides saved selection/new/name/save/delete, fan selector and automatic/fixed/sensor editor; existing curve preview can serve sensor mode. Badge `未应用` and one short note explain hardware unconnected. Dirty edit must not be silently overwritten by selection, create/delete, closing, polling or conflicts. Use textContent/DOM APIs for user names, no innerHTML. Persist scope/sort plus display unit/precision in a small on-demand settings area (can use same modal). Serialize backend writes and retain drafts on failures. Browser/no IPC must not claim persistent success; no localStorage mock fallback.

Backend owns Rust store/main/Cargo. Frontend owns ui/ and scripts/test* UI tests. Parent owns release versions/docs/integration. No actor overwrites the others' owned files.
