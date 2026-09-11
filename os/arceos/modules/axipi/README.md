# axipi

[![Crates.io](https://img.shields.io/crates/v/axipi)](https://crates.io/crates/axipi)
[![Docs.rs](https://docs.rs/axipi/badge.svg)](https://docs.rs/axipi)

[ArceOS](https://github.com/arceos-org/arceos) Inter-Processor Interrupt (IPI) management module.

Provides one coalesced physical IPI edge for typed logical owners, plus bounded
synchronous hard-IRQ calls to a specific CPU. Subsystems retain and drain their
own pending state; the IPI transport does not allocate callback payloads or own
multicast work.

## License

This project is licensed under GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0.
