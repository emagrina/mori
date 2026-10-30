#!/usr/bin/env python3
"""Build a signed universal sandboxed test app. All fixtures are synthetic or repo-owned.
Requires a real Apple signing identity belonging to AYA5S225VU; never ad-hoc auth bypasses.
"""
import argparse, json, os, plistlib, shutil, struct, subprocess, tempfile, time, zlib
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]
def run(args, **kw):
    return subprocess.run([str(a) for a in args], check=True, **kw)
def fixtures(res):
    res.mkdir(parents=True,exist_ok=True)
    def chunk(t,b): return struct.pack('>I',len(b))+t+b+struct.pack('>I',zlib.crc32(t+b))
    png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',32,16,8,2,0,0,0))+chunk(b'IDAT',zlib.compress((b'\0'+bytes([200,80,20])*32)*16))+chunk(b'IEND',b'')
    (res/'tiny.png').write_bytes(png);(res/'empty').write_bytes(b'');(res/'junk').write_bytes(b'junk')
    for fmt in ['jpeg','gif']:
        run(['sips','-s','format',fmt,res/'tiny.png','--out',res/f'tiny.{fmt}'],stdout=subprocess.DEVNULL)
    (res/'meta').write_bytes(b'FILE'+png);(res/'metabatch').write_bytes(struct.pack('<I',len(png)+4)+b'FILE'+png)
    bomb=bytearray(png);bomb[16:20]=struct.pack('>I',100000);bomb[29:33]=struct.pack('>I',zlib.crc32(bomb[12:29]));(res/'bomb.png').write_bytes(bomb)
    shutil.copy(ROOT/'tests/fixtures/tiny.heic',res/'tiny.heic')
    # Probe consumes the payload of the moov atom, not a file path or whole MP4.
    mp4=(ROOT/'tests/fixtures/rotated-90.mp4').read_bytes();at=0
    while at+8<=len(mp4):
        length=int.from_bytes(mp4[at:at+4],'big');tag=mp4[at+4:at+8]
        if length<8: raise ValueError('invalid fixture')
        if tag==b'moov': (res/'probe').write_bytes(b'MP4\0'+mp4[at+8:at+length]);break
        at+=length
    assert (res/'probe').exists()
    objs=['<< /Type /Catalog /Pages 2 0 R >>','<< /Type /Pages /Kids [3 0 R] /Count 1 >>','<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>','<< /Length 34 >>\nstream\nBT /F1 40 Tf 10 30 Td (Mori) Tj ET\nendstream','<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>']
    pdf=b'%PDF-1.4\n';offsets=[]
    for i,o in enumerate(objs):offsets.append(len(pdf));pdf+=f'{i+1} 0 obj\n{o}\nendobj\n'.encode()
    xref=len(pdf);pdf+=f'xref\n0 {len(objs)+1}\n0000000000 65535 f \n'.encode()
    for off in offsets:pdf+=f'{off:010} 00000 n \n'.encode()
    pdf+=f'trailer\n<< /Size {len(objs)+1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode()
    (res/'tiny.pdf').write_bytes(pdf);(res/'pdfpage').write_bytes(struct.pack('<I',1)+pdf)

def main():
    p=argparse.ArgumentParser();p.add_argument('--identity',required=True);p.add_argument('--testing',action='store_true',help='debug-only fault injection and denial probes');p.add_argument('--output',type=Path);p.add_argument('--rosetta',action='store_true',help='also run the universal host as x86_64');args=p.parse_args()
    output=args.output or Path(tempfile.mkdtemp(prefix='mori-xpc-smoke-'))
    output.mkdir(parents=True,exist_ok=True)
    app=output/'MoriXPCTest.app'
    if app.exists():raise SystemExit('Use a new output directory; existing test bundles are not overwritten')
    service=app/'Contents/XPCServices/app.mori.viewer.decoder.xpc'; exe=app/'Contents/MacOS/probe'
    exe.parent.mkdir(parents=True);(service/'Contents/MacOS').mkdir(parents=True)
    fixtures(app/'Contents/Resources')
    info={'CFBundleIdentifier':'app.mori.viewer','CFBundleExecutable':'probe','CFBundlePackageType':'APPL','CFBundleVersion':'1','LSMinimumSystemVersion':'12.0'}
    (app/'Contents/Info.plist').write_bytes(plistlib.dumps(info));shutil.copy(ROOT/'xpc/Info.plist',service/'Contents/Info.plist')
    profile='debug' if args.testing else 'release'
    env={**os.environ,'MACOSX_DEPLOYMENT_TARGET':'12.0'}
    for arch in ['aarch64-apple-darwin','x86_64-apple-darwin']:
        options=['--features','xpc-testing'] if args.testing else ['--release']
        run(['cargo','build','--locked','--manifest-path',ROOT/'decoder/Cargo.toml','--target',arch,*options],env=env)
    run(['lipo','-create',*[ROOT/f'decoder/target/{arch}/{profile}/mori-decoder' for arch in ['aarch64-apple-darwin','x86_64-apple-darwin']],'-output',service/'Contents/MacOS/mori-decoder'])
    run(['xcrun','clang','-fobjc-arc','-fblocks','-arch','arm64','-arch','x86_64','-mmacosx-version-min=12.0','-framework','Foundation','-framework','Security',ROOT/'xpc/bridge.m',ROOT/'xpc/smoke-client.m','-o',exe])
    for binary in [exe,service/'Contents/MacOS/mori-decoder']:
        assert set(subprocess.check_output(['lipo','-archs',str(binary)],text=True).split())=={'arm64','x86_64'}
    def sign(path):run(['codesign','--force','--options','runtime','--timestamp=none','--sign',args.identity,'--entitlements',ROOT/'xpc/Entitlements.plist',path])
    sign(service);sign(app);run(['codesign','--verify','--deep','--strict',app])
    report=[]
    def check(op,file,expected=0,prefix=(),bundle=app,extra=()):
        start=time.monotonic();result=subprocess.run([*prefix,str(bundle/'Contents/MacOS/probe'),op,file,*extra],capture_output=True,timeout=60)
        row={'op':op,'fixture':file,'status':result.returncode,'bytes':len(result.stdout),'seconds':round(time.monotonic()-start,3),'host':'x86_64' if prefix else 'native'}
        report.append(row);print(json.dumps(row),flush=True)
        assert result.returncode==expected, result.stderr.decode()
        if expected==0:
            assert result.stdout[:4]==b'MORI' and result.stdout[13:16]==b'\0\0\0'
            if op in ['thumb','preview','frame','pdfpage','_restart','_memoryrestart']:
                assert result.stdout[12]==2 and result.stdout[16:24]==b'\x89PNG\r\n\x1a\n'
                w,h=struct.unpack('>II',result.stdout[32:40]);assert 0<w<=128 and 0<h<=128
        return result.stdout
    for prefix in ([(),('arch','-x86_64')] if args.rosetta else [()]):
        for op,file in [('thumb','tiny.png'),('thumb','tiny.jpeg'),('preview','tiny.gif'),('preview','tiny.png'),('frame','tiny.png'),('fingerprint','tiny.png'),('thumb','tiny.heic'),('pdfinfo','tiny.pdf'),('pdfpage','pdfpage'),('meta','meta'),('metabatch','metabatch'),('sanitize','tiny.png'),('probe','probe')]:check(op,file,prefix=prefix)
    check('thumb','junk',2);check('thumb','bomb.png',3);check('invalid','empty',6);check('_badversion','empty',6)
    check('thumb','tiny.png',3,extra=('1000','0'))
    if args.testing:
        security=json.loads(check('_security','empty')[16:]);assert all(security.values()),security
        report.append({'isolation':security})
        check('_restart','empty');check('_memoryrestart','empty')
    else:
        check('_security','empty',6);check('_hang','empty',6);check('_memory','empty',6)
    # Same team, wrong app identifier must NOT be admitted, even with a valid embedded service.
    other=output/'Untrusted.app';shutil.copytree(app,other)
    info['CFBundleIdentifier']='app.mori.viewer.xpc-test-untrusted';(other/'Contents/Info.plist').write_bytes(plistlib.dumps(info));sign(other)
    check('thumb','tiny.png',7,bundle=other)
    # Missing service: no in-process or standalone fallback.
    parked=output/'parked-decoder.xpc';shutil.move(service,parked);sign(app)
    check('thumb','tiny.png',7)
    shutil.move(parked,service);sign(app)
    (output/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    print(f'PASS: {output}/report.json',flush=True)
if __name__=='__main__':main()
