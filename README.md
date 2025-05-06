# Resource namespace module for [ArceOS](https://github.com/arceos-org/arceos)

This was originally introduced by [arceos#224](https://github.com/arceos-org/arceos/pull/224), but it had limited functionality and issues with memory leaks, so the one here is an almost complete rewrite. And we split it from the main repo.

## Why Do We Need This?

Namespaces are used to control system resource sharing between threads. This module provides a unified interface to access system resources in different scenarios.

For a unikernel, there is only one global namespace, so all threads share the same system resources, such as virtual address space, working directory, and file descriptors, etc.

For a monolithic kernel, each process corresponds to a namespace, all threads in the same process share the same system resources. Different processes have different namespaces and isolated resources.

For further container support, some global system resources can also be grouped into a namespace.
