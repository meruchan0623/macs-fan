#!/usr/bin/env python3
"""Embed a compiled, signing-gated read-only daemon into a build artifact.

No service registration, root execution, launchctl, keychain or installation.
"""
from pathlib import Path
import argparse, hashlib, json, os, plistlib, shutil, stat, subprocess, tempfile
ROOT=Path(__file__).resolve().parents[1]

def run(args, **kwargs):
    print('+', ' '.join(map(str,args)), flush=True)
    subprocess.run(list(map(str,args)), check=True, **kwargs)

def is_install_location(path):
    # macOS firmlinks preserve textual Data-volume aliases through resolve().
    # Compare directory identity as well as spelling, including user installs.
    roots=[Path('/Applications'),Path('/System/Applications'),Path.home()/'Applications']
    ancestors=[path,*path.parents]
    for parent in ancestors:
        for root in roots:
            if parent==root:return True
            try:
                if parent.samefile(root):return True
            except FileNotFoundError:
                continue
    return False

def atomic_copy(source, destination):
    # Replace the directory entry instead of truncating an existing hard link.
    temporary=None
    try:
        with tempfile.NamedTemporaryFile(prefix='.macs-fan-stage-', dir=destination.parent, delete=False) as output:
            temporary=Path(output.name)
            with source.open('rb') as input_file:shutil.copyfileobj(input_file, output)
            os.fchmod(output.fileno(), stat.S_IMODE(source.stat().st_mode))
            output.flush();os.fsync(output.fileno())
        os.replace(temporary,destination)
        temporary=None
    finally:
        if temporary is not None:temporary.unlink(missing_ok=True)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app',type=Path,required=True)
    parser.add_argument('--target',type=Path)
    parser.add_argument('--signing-identity',default='-')
    args=parser.parse_args()
    app=args.app.resolve()
    if os.geteuid()==0:raise SystemExit('Refusing root; staging is an unprivileged build operation')
    if is_install_location(app):raise SystemExit('Stage a build artifact, not an installed application')
    info=plistlib.loads((app/'Contents/Info.plist').read_bytes())
    if info.get('CFBundleIdentifier')!='local.macsfan.desktop':raise SystemExit('Unexpected application identity')
    target=(args.target or Path(tempfile.mkdtemp(prefix='macs-fan-service-build-'))).resolve()
    if is_install_location(target):raise SystemExit('Build target must not be an application installation directory')
    # Keep Mach-O alignment intact on Rust toolchains affected by LLVM #203678.
    # This also covers host proc-macros; do not patch or re-sign compiler dylibs.
    env=dict(os.environ,CARGO_TARGET_DIR=str(target),CARGO_INCREMENTAL='0',
             MACOSX_DEPLOYMENT_TARGET='13.0',CARGO_PROFILE_RELEASE_STRIP='none')
    run(['cargo','build','--release','--offline','--no-default-features','--manifest-path',ROOT/'Cargo.toml'],env=env)
    staged=Path(tempfile.mkdtemp(prefix='macs-fan-service-stage-'))
    helper=staged/'MacsFanHelper'
    run(['xcrun','clang','-fobjc-arc','-fblocks','-Wall','-Wextra','-Werror','-mmacosx-version-min=13.0',
         '-framework','Foundation','-framework','Security','-framework','IOKit','-framework','AppKit',
         ROOT/'native/Helper.m',ROOT/'native/ProductionIdentity.m',
         target/'release/libmacs_fan_helper_bridge.a','-o',helper])
    run(['codesign','--force','--sign',args.signing_identity,'--options','runtime','--identifier','local.macsfan.helper.control',helper])
    run(['codesign','--verify','--strict','--verbose=2',helper])
    launch=app/'Contents/Library/LaunchServices'; daemons=app/'Contents/Library/LaunchDaemons'
    for destination in (launch,daemons):
        current=app
        for component in destination.relative_to(app).parts:
            current=current/component
            if current.is_symlink():raise SystemExit('Refusing symlink bundle destination')
            if current.exists() and not current.is_dir():raise SystemExit('Bundle destination is not a directory')
            current.mkdir(exist_ok=True)
    for destination in (launch/'MacsFanHelper', daemons/'local.macsfan.helper.control.plist'):
        if destination.is_symlink() or (destination.exists() and not destination.is_file()):
            raise SystemExit('Refusing nonregular bundle output')
    atomic_copy(helper,launch/'MacsFanHelper')
    atomic_copy(ROOT/'packaging/local.macsfan.helper.control.plist',daemons/'local.macsfan.helper.control.plist')
    run(['codesign','--force','--sign',args.signing_identity,'--options','runtime',app])
    run(['codesign','--verify','--deep','--strict','--verbose=2',app])
    result={'app':str(app),'version':info.get('CFBundleShortVersionString'),
            'serviceRegistered':False,'hardwareWriteEnabled':False,
            'signatureKind':'adhoc' if args.signing_identity=='-' else 'requested_identity',
            'helperSha256':hashlib.sha256((launch/'MacsFanHelper').read_bytes()).hexdigest(),
            'plistSha256':hashlib.sha256((daemons/'local.macsfan.helper.control.plist').read_bytes()).hexdigest()}
    (staged/'staging.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result,indent=2));print('staging_record='+str(staged/'staging.json'))
if __name__=='__main__':main()
