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

1. Enable testcases in the `judge_**.py` script. You can find `TODO` fields in the script and add the new testcases in the corresponding place. The format of the command can be seen in [judge](https://github.com/Azure-stars/oskernel-testsuits-cooperation/tree/master/judge).

2. If the testcase belongs to `basic`, you need to add it in the `./scripts/oscomp_test.sh`. You can find `TODO` fields in the script and add the new testcases in the corresponding place. An example to add `clone` testcases are shown below:

```shell
# TODO: add more basic testcases
basic_testlist=(
    "/$LIBC/basic/brk"
    "/$LIBC/basic/chdir"
    "/$LIBC/basic/clone"    # add the new testcase here
)
```