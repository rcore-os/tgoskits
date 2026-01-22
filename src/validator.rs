//! Validator utilities for the crate interface.

use syn::{Error, FnArg, Signature};

use crate::errors::generic_not_allowed_error;

/// Validate the function signature, rejecting generic parameters and receivers.
///
/// Returns `Err(Error)` with a compile error if:
/// - The function has generic parameters
/// - Any argument is a receiver (`self`, `&self`, `&mut self`)
pub fn validate_fn_signature(sig: &Signature) -> Result<(), Error> {
    if !sig.generics.params.is_empty() {
        return Err(generic_not_allowed_error(&sig.generics));
    }

    for arg in &sig.inputs {
        if let FnArg::Receiver(receiver) = arg {
            return Err(Error::new_spanned(
                receiver,
                "methods with receiver (self) are not allowed in crate_interface",
            ));
        }
    }
    Ok(())
}
