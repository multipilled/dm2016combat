"""vtdiff.py <exe> <n> <vtA> [<vtB> ...]: print n vtable slots of each vtable side by side; '*' marks slots differing from A."""
import struct, sys, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
def q(va):
    return struct.unpack_from("<Q", d, pe.get_offset_from_rva(va - base))[0]
n = int(sys.argv[2])
vts = [int(x, 16) for x in sys.argv[3:]]
for i in range(n):
    row = [q(v + 8 * i) for v in vts]
    print(f"{i:3d} +{8*i:#05x} " + " ".join(f"{x:#x}{'*' if x != row[0] else ' '}" for x in row))
