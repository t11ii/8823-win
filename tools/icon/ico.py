# Rewrite an ICO so that 64px and up are stored as PNG (smaller exe); small sizes stay BMP.
import struct,sys
src,pngdir,out=sys.argv[1:4]
d=open(src,'rb').read();n=struct.unpack('<H',d[4:6])[0]
ents=[]
for i in range(n):
    e=d[6+16*i:22+16*i];w=e[0] or 256;sz,off=struct.unpack('<II',e[8:16])
    blob=open(f'{pngdir}/w{w}.png','rb').read() if w>=64 else d[off:off+sz]
    ents.append((e[:8],blob))
hdr=struct.pack('<HHH',0,1,n);off=6+16*n;dirs=b'';data=b''
for e,b in ents:
    dirs+=e+struct.pack('<II',len(b),off+len(data));data+=b
open(out,'wb').write(hdr+dirs+data)
