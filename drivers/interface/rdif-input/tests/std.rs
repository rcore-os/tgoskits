use rdif_input::{InputError, io};

#[test]
fn rdif_input_errors_map_to_io_kinds() {
    assert!(matches!(
        io::ErrorKind::from(InputError::NotSupported),
        io::ErrorKind::Unsupported
    ));
    assert!(matches!(
        io::ErrorKind::from(InputError::Again),
        io::ErrorKind::Interrupted
    ));
    assert!(matches!(
        io::ErrorKind::from(InputError::NotAvailable),
        io::ErrorKind::NotAvailable
    ));
    assert!(matches!(
        io::ErrorKind::from(InputError::InvalidEvent),
        io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        io::ErrorKind::from(InputError::Other("input backend".into())),
        io::ErrorKind::Other(_)
    ));
}
