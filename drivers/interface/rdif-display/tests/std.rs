use rdif_display::{DisplayError, io};

#[test]
fn rdif_display_errors_map_to_io_kinds() {
    assert!(matches!(
        io::ErrorKind::from(DisplayError::NotSupported),
        io::ErrorKind::Unsupported
    ));
    assert!(matches!(
        io::ErrorKind::from(DisplayError::InvalidFramebuffer),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(DisplayError::NotAvailable),
        io::ErrorKind::NotAvailable
    ));
    assert!(matches!(
        io::ErrorKind::from(DisplayError::Other("display backend".into())),
        io::ErrorKind::Other(_)
    ));
}
