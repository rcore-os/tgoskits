/* ACPICA Rust/C ABI query, Apache-2.0. */
#include "acpi.h"
unsigned int acpica_interpreter_abi_width(void) { return sizeof(ACPI_SIZE) * 8; }
