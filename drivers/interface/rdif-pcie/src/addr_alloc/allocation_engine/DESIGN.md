# Design

## Allocation engine

This implementation uses an interval tree that is specialized for allocation of
memory-mapped I/O and port I/O address space. The fields of the structures
defined will have semantic meaning for this context (e.g. node state to indicate
if a node in the tree is assigned or not to a device).

We offer three options for placing a memory slot in the managed address space:

1. `LastMatch` -> When using this allocation policy the allocator will try to
insert the range described by the constraint at the first available position
starting from the end of the managed address space.
2. `FirstMatch` -> When using this allocation policy the allocator will try to
insert the range described by the constraint at the first available position
starting from the beginning of the managed address space.
3. `ExactMatch(u64)` -> When using this allocation policy the allocator will
try to insert the range at the exact position described by the constraint,
otherwise it will return an error.

Struct `Constraint` is used to describe the overall information of the resource
needed to be allocated. This structure is also used by `IntervalTree` to know
where and how to allocate the resource. The fields that are mandatory for
allocating a new memory slot are size of the slot, alignment of the slot and
allocation policy. Optionally the user can specify a range where the allocator
will place the allocated memory slot.

## Interval tree

An interval tree is a tree data structure used for storing information about
intervals. Specifically, it allows one to efficiently identify intervals that
are overlapping with a given point, or another interval. We considered that
this characteristic makes this data structure appropriate to be used as an
allocation engine for memory slots inside an address space. The time complexity
of an interval tree, namely `O(log ⁡n+m)` for queries, `O(log n)` for creation
and `O(log n)` for insertion and deletion of nodes. The key of each node of the
tree is represented using an inclusive range that contains the bounds of the
address space. Each node in the tree can have two states, either `Free` or
`Allocated`. Beside the information presented above the representation of a
node also contains references two the children node of the current node.

## Usage

To use the `IntervalTree` implementation as an address allocator one should
first create an interval tree object and give an address space as a root node.
After, the user should create a constraint with the size for the resource.
Optionally the constraint could also contain the address alignment and the
first address to be reserved.

## State transition

At the beginning, the interval tree will contain just one node that will
represent the whole address space, the state of this node will be free.

![IntervalTree creation example](/images/first_node.png)

When we allocate a memory slot, one of the nodes that have the state free
will be split accordingly. A new node that has as the key a range representing
the allocated memory slot will be inserted in the tree.

![Node Allocation example](/images/interval_tree_allocation.png)

When one of the allocated nodes is freed its state will be changed from
`NodeState::Allocated` to `NodeState::Free` if there are two adjacent nodes
that are not allocated then they will be merged in a single node.

![Node Freeing example](/images/after_free.png)

## Address aligning

Memory slots allocated using address allocator can have the start aligned to a
specified value. We offer the possibility to align the starting address either
to the next boundary or the previous one.

If the allocation policy used is `AllocPolicy::FirstMatch` all memory slots
will start at an address aligned to the first multiple of specified alignment
value that is greater or equal to the candidate address.

Example:

```text
initial_address              = 0b0..01000000000000000000000000000001
alignment                    = 0b0..00000000000000000001000000000000
initial_addr + alignment - 1 = 0b0..01000000000000000001000000000000
!(alignment - 1)             = 0b1..11111111111111111111000000000000
```

The aligned address will be the result of bitwise AND between
`initial address + alignment - 1` and bitwise NOT of `alignment - 1`.

```text
0b0..01000000000000000000000000000001(0x0000000040001000)&
0b1..11111111111111111111000000000000(0xFFFFFFFFFFFFF000)
-------------------------------------------------------------------
0b0..01000000000000000001000000000000(0x0000000040001000)
```

If the allocation policy used is `AllocPolicy::LastMatch` all memory slots will
start at an address aligned to the first multiple of specified alignment value
that is lower or equal to the candidate address.

Example:

```text
initial_address  = 0b0..01000000000000000000000000000001
alignment        = 0b0..00000000000000000001000000000000
!(alignment - 1) = 0b1..11111111111111111111000000000000
```

The aligned address will be the result of bitwise AND between `initial address` and
bitwise NOT of `alignment - 1`.

```text
0b0..01000000000000000000000000000001(0x0000000040000001)&
0b1..11111111111111111111000000000000(0xFFFFFFFFFFFFF000)
-------------------------------------------------------------------
0b0..01000000000000000000000000000000(0x0000000040000000)
```
