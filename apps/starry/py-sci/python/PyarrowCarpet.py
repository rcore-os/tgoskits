#!/usr/bin/env python3
# PyarrowCarpet.py — Apache Arrow / Parquet I/O correctness carpet on musl-native CPython.
#
# The core is the PROVEN (4-arch green) Parquet write -> read -> exact round-trip equality
# assertion (schema field names/types AND every value), ported verbatim. It exercises the
# heavy Arrow C++ stack on musl (libarrow / libparquet / libthrift / libprotobuf + Snappy/Zstd
# codecs + mmap/threads/file I/O of the columnar Parquet format). A few conservative, API-stable
# additions (compute kernels, more Arrow types, RecordBatch, ChunkedArray, in-memory IPC stream
# round-trip) widen the surface using only long-stable pyarrow APIs. Version gate lenient
# (major >= 4). Self-contained ok/fail counters; prints PYARROW_RESULT then PYARROW_DONE only
# when fail == 0.
import os
import sys

ok = 0
fail = 0


def chk(name, cond, info=""):
    global ok, fail
    if cond:
        ok += 1
        print("  ok %s%s" % (name, (" " + info) if info else ""))
    else:
        fail += 1
        print("  FAIL %s%s" % (name, (" " + info) if info else ""))


import pyarrow as pa
import pyarrow.parquet as pq

chk("version", int(pa.__version__.split(".")[0]) >= 4, "pyarrow=%s" % pa.__version__)

# ---- PROVEN core (4-arch green): Parquet write -> read -> EXACT round-trip equality ----
ids = pa.array([1, 2, 3, 4, 5], type=pa.int32())
bigs = pa.array([100, 200, 300, 400, 500], type=pa.int64())
names = pa.array(["alice", "bob", "carol", "dave", "erin"], type=pa.string())
scores = pa.array([1.5, 2.5, 3.5, 4.5, 5.5], type=pa.float64())
tags = pa.array(["x", "yy", "zzz", "", "w"], type=pa.string())
table = pa.table({"id": ids, "big": bigs, "name": names, "score": scores, "tag": tags})
chk("table_build", table.num_rows == 5 and table.num_columns == 5)

path = "/tmp/t.parquet"
pq.write_table(table, path)
sz = os.path.getsize(path)
back = pq.read_table(path)
chk("parquet_file_nonempty", sz > 0, "%d bytes" % sz)
chk("parquet_schema_equal", table.schema.equals(back.schema))
chk("parquet_values_equal", table.equals(back))
# Spot-check actual cell values (defense in depth, independent of .equals()).
chk("parquet_cells_id", back.column("id").to_pylist() == [1, 2, 3, 4, 5])
chk("parquet_cells_big", back.column("big").to_pylist() == [100, 200, 300, 400, 500])
chk("parquet_cells_name", back.column("name").to_pylist() == ["alice", "bob", "carol", "dave", "erin"])
chk("parquet_cells_score", back.column("score").to_pylist() == [1.5, 2.5, 3.5, 4.5, 5.5])
chk("parquet_cells_tag", back.column("tag").to_pylist() == ["x", "yy", "zzz", "", "w"])
# Schema field types are exactly what we declared (string of the type is stable).
ftypes = [str(f.type) for f in back.schema]
chk("parquet_field_types", ftypes == ["int32", "int64", "string", "double", "string"], "%s" % ftypes)

# ---- compute kernels (long-stable pyarrow.compute API) ----
import pyarrow.compute as pc

chk("compute_sum", pc.sum(ids).as_py() == 15)
chk("compute_add", pc.add(pa.array([1, 2, 3]), pa.array([10, 20, 30])).to_pylist() == [11, 22, 33])
chk("compute_equal", pc.equal(pa.array([1, 2, 3]), pa.array([1, 9, 3])).to_pylist() == [True, False, True])
chk("compute_min_max",
    pc.min(scores).as_py() == 1.5 and pc.max(scores).as_py() == 5.5)

# ---- more Arrow types: bool / list ----
b = pa.array([True, False, True, None], type=pa.bool_())
chk("bool_array", b.to_pylist() == [True, False, True, None] and b.null_count == 1)
li = pa.array([[1, 2], [3], []], type=pa.list_(pa.int64()))
chk("list_array", li.to_pylist() == [[1, 2], [3], []])

# ---- RecordBatch from arrays ----
rb = pa.RecordBatch.from_arrays([pa.array([1, 2, 3]), pa.array(["a", "b", "c"])], names=["n", "s"])
chk("recordbatch", rb.num_rows == 3 and rb.num_columns == 2
    and rb.column(0).to_pylist() == [1, 2, 3] and rb.column(1).to_pylist() == ["a", "b", "c"])

# ---- ChunkedArray ----
ca = pa.chunked_array([[1, 2], [3, 4, 5]])
chk("chunked_array", ca.length() == 5 and ca.num_chunks == 2 and ca.to_pylist() == [1, 2, 3, 4, 5])

# ---- in-memory Arrow IPC stream round-trip (pure pyarrow, no pandas) ----
sink = pa.BufferOutputStream()
with pa.ipc.new_stream(sink, table.schema) as writer:
    writer.write_table(table)
buf = sink.getvalue()
ipc_back = pa.ipc.open_stream(buf).read_all()
chk("ipc_roundtrip", ipc_back.equals(table))

print("PYARROW_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("PYARROW_DONE")
    sys.exit(0)
sys.exit(1)
