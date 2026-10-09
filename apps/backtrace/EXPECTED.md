# Expected target output

所有 demo 都由 target 自己输出回溯协议。map 就绪时，每个符号化帧包含函数
名和 `file:line`；map 尚未就绪时允许只有地址。

```text
BACKTRACE_BEGIN kind=panic arch=x86_64 alloc=false map=true
BT 0 ip=0x... fp=0x... symbol=... at ...:...
BACKTRACE_END
```

StarryOS memtrack demo 还应输出：

```text
Memory allocation sample recorded
Hard memory allocation sample recorded
BACKTRACE_BEGIN kind=alloc ...
STARRY_MEMTRACK_BACKTRACE_OK
```

输出中不应出现宿主符号化段。
