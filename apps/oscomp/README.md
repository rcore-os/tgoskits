# OSCOMP testcases

This directory contains testcases for the OSCOMP application.

## Testcases list

The [testcase_list](./testcase_list) file contains the list of testcases that will be run after StarryOS is built. Every line in the file is the path to a testcase. An example of the file is shown below:

```
musl/basic/brk
musl/basic/exit
```

Then Starry will find `musl/basic/brk` from the sdcard images.And the working directory in the sdcard is default `/`.

## Judge scripts

Files like `judge_**.py` are the scripts that judge whether the output of the kernel is correct or not. The format ot the scripts can be seen at [judge](https://github.com/Azure-stars/oskernel-testsuits-cooperation/tree/master/judge).

When run `make oscomp_test` in the root directory, the testcases in the `testcase_list` will be run and the result will be shown in the terminal. And these scripts will be run to judge whether the output is correct or not.

## How to add new testcases

### Add basic testcases

If we want to add `write` testcase, then we need:

1. Enable testcases in the [judge_basic.py](./judge_basic.py)。An example is that if the `target_testcase` in [judge_basic.py](./judge_basic.py) is below:

   ```sh
   target_testcases = [
       "test_brk",
       "test_chdir",
       "test_execve",
       "test_pipe",
   ]
   ```

   Then we need to enable `write` testcase by rewriting it as 

   ```sh
   target_testcases = [
       "test_brk",
       "test_chdir",
       "test_execve",
       "test_pipe",
       "test_write",
   ]
   ```

2. Add the testcase in the [oscomp_test.sh](../../scripts/oscomp_test.sh)。An example is that if the `basic_testlist` in [oscomp_test.sh](../../scripts/oscomp_test.sh)  is below:

   ```sh
   # TODO: add more basic testcases
   basic_testlist=(
       "/$LIBC/basic/brk"
       "/$LIBC/basic/chdir"
       "/$LIBC/basic/clone"    # add the new testcase here
   )
   ```

   Then we need to enable `write` testcase by rewriting it as 

   ```sh
   # TODO: add more basic testcases
   basic_testlist=(
       "/$LIBC/basic/brk"
       "/$LIBC/basic/chdir"
       "/$LIBC/basic/clone"
       "/$LIBC/basic/write"
   )
   ```

### Add libc testcases

The standard of the  [judge_libctest.py](./judge_libctest.py) can be seen at [judge-std](https://github.com/Azure-stars/oskernel-testsuits-cooperation/tree/master/judge/judge_libctest.py).

If we want to add `entry-static.exe basename` testcase, and the `libctest_baseline` in the [judge_libctest.py](./judge_libctest.py) is as below:

```python
libctest_baseline = """
========== START entry-static.exe argv ==========
Pass!
========== END entry-static.exe argv ==========
"""
```

Then we need to enable `entry-static.exe basename` testcase by rewriting it as 

```python
libctest_baseline = """
========== START entry-static.exe argv ==========
Pass!
========== END entry-static.exe argv ==========
========== START entry-static.exe basename ==========
Pass!
========== END entry-static.exe basename ==========
"""
```

That is, we **append the instructions or results used in the evaluation** to the script.

### Add busybox testcases

The standard of the [judge_busybox.py](./judge_busybox.py) can be seen at [judge-std](https://github.com/Azure-stars/oskernel-testsuits-cooperation/tree/master/judge/judge_busybox.py).

If we want to add `echo "#### independent command test"` testcase, and the `busybox_baseline` in the [judge_busybox.py](./judge_busybox.py) is as below:

```python
cmd = """"""
```

Then we need to enable `echo "#### independent command test"` testcase by rewriting it as 

```python
cmd = """
echo "#### independent command test"
"""
```

### Add lua testcases

The standard of the [judge_lua.py](./judge_lua.py) can be seen at [judge-std](https://github.com/Azure-stars/oskernel-testsuits-cooperation/tree/master/judge/judge_lua.py).

If we want to add `max_min.lua` testcase, and the `cmd` in the [judge_lua.py](./judge_lua.py) is as below:

```python
cmds = """"""
```

Then we need to enable `max_min.lua` testcase by rewriting it as 

```python
cmds = """
max_min.lua
"""
```


### About iozone testcases

IOZONE is a special testcase, and the standard of the [judge_iozone.py](./judge_iozone.py) can be seen at [judge-std](https://github.com/Azure-stars/oskernel-testsuits-cooperation/tree/master/judge/judge_iozone.py).It measures both correctness and performance.

So we will directly enable all its subtest cases by rewriting `iozone_baseline` in the [judge_iozone.py](./judge_iozone.py) to that in the [judge-std](https://github.com/Azure-stars/oskernel-testsuits-cooperation/tree/master/judge/judge_iozone.py).