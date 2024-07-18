/// Functions for the Debug Console extension
#[derive(Copy, Clone, Debug)]
pub enum DebugConsoleFunction {
    /// Prints the given string to the system console.
    PutString {
        /// The length of the string to print.
        len: u64,
        /// The address of the string.
        addr: u64,
    },
}
