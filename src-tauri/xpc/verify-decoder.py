#!/usr/bin/env python3
"""Fail closed if the Store decoder is absent, stale, unsigned or over-entitled."""
import argparse, hashlib, json, plistlib, subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
STAGE=ROOT/'target/store-decoder'
NAME='app.mori.viewer.decoder.xpc'
SOURCES=['decoder/Cargo.toml','decoder/Cargo.lock','decoder/build.rs','decoder/src/main.rs',
         'xpc/bridge.m','xpc/Info.plist','xpc/Entitlements.plist','src/worker.rs','src/heif.rs','src/pdf.rs',
         'src/metadata.rs','src/sanitize.rs','src/probe.rs','src/i18n.rs']
def source_hash():
    h=hashlib.sha256()
    for name in sorted(SOURCES+[str(p.relative_to(ROOT)) for p in (ROOT/'locales').glob('*.json')]):
        h.update(name.encode()+b'\0');h.update((ROOT/name).read_bytes());h.update(b'\0')
    return h.hexdigest()
def command(args):
    p=subprocess.run([str(a) for a in args],capture_output=True)
    if p.returncode:raise RuntimeError(p.stderr.decode() or p.stdout.decode())
    return p.stdout

def verify(service):
    binary=service/'Contents/MacOS/mori-decoder'
    if set(command(['lipo','-archs',binary]).decode().split())!={'arm64','x86_64'}:raise RuntimeError('Decoder is not universal')
    command(['codesign','--verify','--strict','-R','=anchor apple generic and identifier "app.mori.viewer.decoder" and certificate leaf[subject.OU] = "AYA5S225VU"',service])
    ent=plistlib.loads(command(['codesign','-d','--entitlements',':-',service]))
    if ent!={'com.apple.security.app-sandbox':True}:raise RuntimeError(f'Unexpected decoder entitlements: {sorted(ent)}')
    info=plistlib.loads((service/'Contents/Info.plist').read_bytes())
    if info!=plistlib.loads((ROOT/'xpc/Info.plist').read_bytes()):raise RuntimeError('Unexpected decoder bundle metadata')
    p=subprocess.run(['codesign','-dvv',str(service)],capture_output=True,text=True,check=True)
    if '(runtime)' not in p.stderr:raise RuntimeError('Decoder lacks hardened runtime')
    return hashlib.sha256(binary.read_bytes()).hexdigest()

def main():
    p=argparse.ArgumentParser();g=p.add_mutually_exclusive_group(required=True);g.add_argument('--prepared',action='store_true');g.add_argument('--record',action='store_true');g.add_argument('--app',type=Path);args=p.parse_args()
    service=(args.app/'Contents/XPCServices'/NAME) if args.app else STAGE/NAME
    binary_hash=verify(service)
    if args.app:
        command(['codesign','--verify','--deep','--strict',args.app])
        ent=plistlib.loads(command(['codesign','-d','--entitlements',':-',args.app]))
        if ent.get('com.apple.security.app-sandbox') is not True:raise RuntimeError('Host is not sandboxed')
        info=plistlib.loads((args.app/'Contents/Info.plist').read_bytes())
        if set(command(['lipo','-archs',args.app/'Contents/MacOS'/info['CFBundleExecutable']]).decode().split())!={'arm64','x86_64'}:raise RuntimeError('Host is not universal')
        if tuple(map(int,info.get('LSMinimumSystemVersion','0').split('.'))) < (12,0):raise RuntimeError('Store host must require macOS 12+')
    else:
        expected={'source_sha256':source_hash(),'binary_sha256':binary_hash}
        stamp=STAGE/'verified-build.json'
        if args.record:stamp.write_text(json.dumps(expected,indent=2)+'\n')
        elif json.loads(stamp.read_text())!=expected:raise RuntimeError('Stale decoder: run xpc/prepare-decoder.sh again')
    print(f'Verified universal, signed, minimally entitled decoder: {service}')
if __name__=='__main__':main()
