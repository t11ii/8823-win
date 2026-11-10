# Pack PNGs into a macOS .icns (PNG-based entries, macOS 10.7+).
import struct, sys
src, out = sys.argv[1:3]
ents = [('icp4', 16), ('icp5', 32), ('ic07', 128), ('ic08', 256), ('ic09', 512), ('ic10', 1024),
        ('ic11', 32), ('ic12', 64), ('ic13', 256), ('ic14', 512)]
body = b''
for t, n in ents:
    d = open(f'{src}/m{n}.png', 'rb').read()
    body += t.encode() + struct.pack('>I', len(d) + 8) + d
open(out, 'wb').write(b'icns' + struct.pack('>I', len(body) + 8) + body)
