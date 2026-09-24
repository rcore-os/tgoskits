use rdif_display::{DisplayError, io};

#[test]
fn display_errors_map_to_io_kinds() {
    assert!(matches!(
        io::ErrorKind::from(DisplayError::Unsupported),
        io::ErrorKind::Unsupported
    ));
    assert!(matches!(
        io::ErrorKind::from(DisplayError::InvalidState),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(DisplayError::NotAvailable),
        io::ErrorKind::NotAvailable
    ));
    assert!(matches!(
        io::ErrorKind::from(DisplayError::Io),
        io::ErrorKind::Other(_)
    ));
}
