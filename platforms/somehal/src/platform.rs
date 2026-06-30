pub fn platform_name() -> Option<&'static str> {
    someboot::platform_name().or_else(|| someboot::rsdp_addr_phys().map(|_| "acpi"))
}
