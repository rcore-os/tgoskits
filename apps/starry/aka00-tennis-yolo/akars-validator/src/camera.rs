#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
