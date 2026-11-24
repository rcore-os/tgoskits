#!/bin/sh
# Builds the seed corpus of every fuzz target from small images made by mke2fs.
#
# Usage: sh make-seeds.sh [corpus-directory]   (default: ./corpus beside this script)
#
# Needs e2fsprogs 1.47 or later (mke2fs, e2fsck, debugfs) and python3. The images use a fixed
# time, so the same e2fsprogs version gives the same seeds. The journal image gets one committed
# transaction written by this script, and the script fails unless e2fsck replays it.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
corpus=${1:-"$here/corpus"}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export E2FSPROGS_FAKE_TIME=1600000000

# A small tree: a file, a symbolic link, a subdirectory with a binary file, and a sparse file
# with enough extents to need an extent index block.
mkdir -p "$work/tree/sub" "$work/htree/many"
printf 'hello\n' > "$work/tree/a.txt"
ln -s a.txt "$work/tree/link"
python3 -c 'import sys; sys.stdout.buffer.write(bytes(range(256)) * 20)' > "$work/tree/sub/b.bin"
python3 - "$work/tree/sparse" <<'EOF'
import sys
with open(sys.argv[1], 'wb') as f:
    for i in range(12):
        f.seek(i * 8192)
        f.write(b'x')
EOF
# A directory with enough entries to need a hash-tree index on a 1 KiB block.
i=1
while [ "$i" -le 150 ]; do
    : > "$work/htree/many/file-with-a-long-name-$i"
    i=$((i + 1))
done

mkfs() { mke2fs -q -F -t ext4 -E root_owner=0:0 "$@"; }
mkfs -b 1024 -O ^has_journal -d "$work/tree" "$work/walk-1k.img" 256k
mkfs -b 4096 -O ^has_journal -d "$work/tree" "$work/walk-4k.img" 512k
mkfs -b 1024 -O ^has_journal,^metadata_csum -d "$work/tree" "$work/walk-1k-nocsum.img" 256k
mkfs -b 1024 -N 256 -O ^has_journal -d "$work/htree" "$work/htree.img" 512k
e2fsck -fyD "$work/htree.img" > /dev/null 2>&1 || [ $? -le 1 ]
mkfs -b 1024 -J size=1 -d "$work/tree" "$work/journal.img" 3m
python3 - "$work" "$corpus" <<'EOF'
import os, re, struct, subprocess, sys

work, corpus = sys.argv[1], sys.argv[2]

def crc32c_raw(crc, data):
    # The CRC32C register without the final inversion, as ext4 stores its checksums.
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82F63B78 if crc & 1 else 0)
    return crc

def debugfs(image, request):
    return subprocess.run(['debugfs', '-R', request, image], capture_output=True, text=True,
                          check=True).stdout

def put(target, name, data):
    os.makedirs(os.path.join(corpus, target), exist_ok=True)
    with open(os.path.join(corpus, target, name), 'wb') as f:
        f.write(data)

def block_size(image):
    return 1024 << struct.unpack_from('<I', image, 1024 + 24)[0]

def blocks_of(path, inside):
    return [int(b) for b in debugfs(path, 'blocks ' + inside).split()]

# One committed transaction in the journal, which rewrites filesystem blocks 400 and 401 with
# 'j'. The journal mke2fs makes has no checksum features, so this is a plain JBD2 v2 log: a
# descriptor block with two tags, the two data blocks, and a commit block.
path = os.path.join(work, 'journal.img')
image = bytearray(open(path, 'rb').read())
size = block_size(image)
journal = blocks_of(path, '<8>')
magic = 0xC03B3998
def header(kind, sequence):
    return struct.pack('>III', magic, kind, sequence)
descriptor = bytearray(size)
first_tag = struct.pack('>IHH', 400, 0, 0) + image[1024 + 0x68:1024 + 0x78]  # 0x68: s_uuid
last_tag = struct.pack('>IHH', 401, 0, 0x2 | 0x8)  # SAME_UUID | LAST_TAG
descriptor[:12 + len(first_tag) + len(last_tag)] = header(1, 1) + first_tag + last_tag
commit = bytearray(size)
commit[:12] = header(2, 1)
for index, block in enumerate([descriptor, b'j' * size, b'j' * size, commit], start=1):
    image[journal[index] * size:(journal[index] + 1) * size] = block
struct.pack_into('>II', image, journal[0] * size + 24, 1, 1)  # s_sequence, s_start
# Mark the filesystem as needing recovery (INCOMPAT_RECOVER) and reseal its superblock checksum.
incompat = struct.unpack_from('<I', image, 1024 + 0x60)[0]
struct.pack_into('<I', image, 1024 + 0x60, incompat | 0x4)
struct.pack_into('<I', image, 1024 + 0x3FC, crc32c_raw(0xFFFFFFFF, image[1024:1024 + 0x3FC]))
open(path, 'wb').write(image)
log = debugfs(path, 'logdump')
if 'Found expected sequence 1, type 2 (commit block)' not in log:
    sys.exit('make-seeds.sh: the journal image has no committed transaction:\n' + log)
replayed = os.path.join(work, 'replayed.img')
with open(replayed, 'wb') as f:
    f.write(image)
subprocess.run(['e2fsck', '-fy', replayed], capture_output=True, check=False)
if open(replayed, 'rb').read()[400 * size:402 * size] != b'j' * 2 * size:
    sys.exit('make-seeds.sh: e2fsck did not replay the journal image')

for name in ['walk-1k', 'walk-4k', 'walk-1k-nocsum', 'htree', 'journal']:
    path = os.path.join(work, name + '.img')
    image = open(path, 'rb').read()
    size = block_size(image)
    superblock = image[1024:2048]
    put('superblock', name, superblock)
    table = (2 if size == 1024 else 1) * size
    put('group_descriptor_table', name, superblock + image[table:table + size])
    if name == 'journal':
        put('journal_replay', name, image)
        continue
    put('htree_lookup' if name == 'htree' else 'mount_and_walk', name, image)
    for inside in ['/', '/sub', '/many'] if name == 'htree' or name.startswith('walk') else []:
        try:
            numbers = blocks_of(path, inside)
        except subprocess.CalledProcessError:
            continue
        for index, number in enumerate(numbers[:4]):
            put('directory_block', f'{name}-{inside.strip("/") or "root"}-{index}',
                image[number * size:(number + 1) * size])
    for inside in ['/a.txt', '/sub', '/sub/b.bin', '/sparse', '/many']:
        location = re.search(r'located at block (\d+), offset 0x([0-9a-f]+)',
                             debugfs(path, 'imap ' + inside))
        if location is None:
            continue
        inode = int(location.group(1)) * size + int(location.group(2), 16)
        put('extent_tree', f'{name}-{inside.strip("/").replace("/", "-")}',
            image[inode + 40:inode + 100])
    # The leaf under the sparse file's index node.
    leaves = re.findall(r'^\s*0/\s*1\s+\d+/\s*\d+\s+\d+\s*-\s*\d+\s+(\d+)',
                        debugfs(path, 'ex /sparse'), re.M)
    for index, number in enumerate(leaves):
        put('extent_tree', f'{name}-sparse-leaf-{index}', image[int(number) * size:(int(number) + 1) * size])
EOF
echo "make-seeds.sh: seeds in $corpus"
