//! Compatibility names for the existing direct USB UVC driver.

pub use uvc_if::descriptors::*;

pub mod request_codes {
    pub const SET_CUR: u8 = uvc_if::descriptors::RequestCode::SetCur as u8;
    pub const GET_CUR: u8 = uvc_if::descriptors::RequestCode::GetCur as u8;
    pub const GET_MIN: u8 = uvc_if::descriptors::RequestCode::GetMin as u8;
    pub const GET_MAX: u8 = uvc_if::descriptors::RequestCode::GetMax as u8;
    pub const GET_RES: u8 = uvc_if::descriptors::RequestCode::GetRes as u8;
    pub const GET_LEN: u8 = uvc_if::descriptors::RequestCode::GetLen as u8;
    pub const GET_INFO: u8 = uvc_if::descriptors::RequestCode::GetInfo as u8;
    pub const GET_DEF: u8 = uvc_if::descriptors::RequestCode::GetDef as u8;
}

pub mod interface_subclass {
    pub const UNDEFINED: u8 = uvc_if::descriptors::InterfaceSubclass::Undefined as u8;
    pub const VIDEO_CONTROL: u8 = uvc_if::descriptors::InterfaceSubclass::VideoControl as u8;
    pub const VIDEO_STREAMING: u8 = uvc_if::descriptors::InterfaceSubclass::VideoStreaming as u8;
    pub const VIDEO_INTERFACE_COLLECTION: u8 =
        uvc_if::descriptors::InterfaceSubclass::VideoInterfaceCollection as u8;
}

pub mod vc_descriptor_subtypes {
    pub const UNDEFINED: u8 = uvc_if::descriptors::VcDescriptorSubtype::Undefined as u8;
    pub const HEADER: u8 = uvc_if::descriptors::VcDescriptorSubtype::Header as u8;
    pub const INPUT_TERMINAL: u8 = uvc_if::descriptors::VcDescriptorSubtype::InputTerminal as u8;
    pub const OUTPUT_TERMINAL: u8 = uvc_if::descriptors::VcDescriptorSubtype::OutputTerminal as u8;
    pub const SELECTOR_UNIT: u8 = uvc_if::descriptors::VcDescriptorSubtype::SelectorUnit as u8;
    pub const PROCESSING_UNIT: u8 = uvc_if::descriptors::VcDescriptorSubtype::ProcessingUnit as u8;
    pub const EXTENSION_UNIT: u8 = uvc_if::descriptors::VcDescriptorSubtype::ExtensionUnit as u8;
}

pub mod vs_descriptor_subtypes {
    pub const UNDEFINED: u8 = uvc_if::descriptors::VsDescriptorSubtype::Undefined as u8;
    pub const INPUT_HEADER: u8 = uvc_if::descriptors::VsDescriptorSubtype::InputHeader as u8;
    pub const OUTPUT_HEADER: u8 = uvc_if::descriptors::VsDescriptorSubtype::OutputHeader as u8;
    pub const STILL_IMAGE_FRAME: u8 =
        uvc_if::descriptors::VsDescriptorSubtype::StillImageFrame as u8;
    pub const FORMAT_UNCOMPRESSED: u8 =
        uvc_if::descriptors::VsDescriptorSubtype::FormatUncompressed as u8;
    pub const FRAME_UNCOMPRESSED: u8 =
        uvc_if::descriptors::VsDescriptorSubtype::FrameUncompressed as u8;
    pub const FORMAT_MJPEG: u8 = uvc_if::descriptors::VsDescriptorSubtype::FormatMjpeg as u8;
    pub const FRAME_MJPEG: u8 = uvc_if::descriptors::VsDescriptorSubtype::FrameMjpeg as u8;
    pub const FORMAT_MPEG2TS: u8 = uvc_if::descriptors::VsDescriptorSubtype::FormatMpeg2Ts as u8;
    pub const FORMAT_DV: u8 = uvc_if::descriptors::VsDescriptorSubtype::FormatDv as u8;
    pub const COLORFORMAT: u8 = uvc_if::descriptors::VsDescriptorSubtype::Colorformat as u8;
    pub const FORMAT_FRAME_BASED: u8 =
        uvc_if::descriptors::VsDescriptorSubtype::FormatFrameBased as u8;
    pub const FRAME_FRAME_BASED: u8 =
        uvc_if::descriptors::VsDescriptorSubtype::FrameFrameBased as u8;
    pub const FORMAT_STREAM_BASED: u8 =
        uvc_if::descriptors::VsDescriptorSubtype::FormatStreamBased as u8;
    pub const FORMAT_H264: u8 = uvc_if::descriptors::VsDescriptorSubtype::FormatH264 as u8;
    pub const FRAME_H264: u8 = uvc_if::descriptors::VsDescriptorSubtype::FrameH264 as u8;
    pub const FORMAT_H264_SIMULCAST: u8 =
        uvc_if::descriptors::VsDescriptorSubtype::FormatH264Simulcast as u8;
}

pub mod video_streaming_controls {
    pub const UNDEFINED: u8 = uvc_if::descriptors::VideoStreamingControl::Undefined as u8;
    pub const PROBE: u8 = uvc_if::descriptors::VideoStreamingControl::Probe as u8;
    pub const COMMIT: u8 = uvc_if::descriptors::VideoStreamingControl::Commit as u8;
    pub const STILL_PROBE: u8 = uvc_if::descriptors::VideoStreamingControl::StillProbe as u8;
    pub const STILL_COMMIT: u8 = uvc_if::descriptors::VideoStreamingControl::StillCommit as u8;
    pub const STILL_IMAGE_TRIGGER: u8 =
        uvc_if::descriptors::VideoStreamingControl::StillImageTrigger as u8;
    pub const STREAM_ERROR_CODE: u8 =
        uvc_if::descriptors::VideoStreamingControl::StreamErrorCode as u8;
    pub const GENERATE_KEY_FRAME: u8 =
        uvc_if::descriptors::VideoStreamingControl::GenerateKeyFrame as u8;
    pub const UPDATE_FRAME_SEGMENT: u8 =
        uvc_if::descriptors::VideoStreamingControl::UpdateFrameSegment as u8;
    pub const SYNC_DELAY: u8 = uvc_if::descriptors::VideoStreamingControl::SyncDelay as u8;
}

pub mod terminal_types {
    pub const TT_VENDOR_SPECIFIC: u16 = uvc_if::descriptors::TerminalType::TtVendorSpecific as u16;
    pub const TT_STREAMING: u16 = uvc_if::descriptors::TerminalType::TtStreaming as u16;
    pub const ITT_VENDOR_SPECIFIC: u16 =
        uvc_if::descriptors::TerminalType::IttVendorSpecific as u16;
    pub const ITT_CAMERA: u16 = uvc_if::descriptors::TerminalType::IttCamera as u16;
    pub const ITT_MEDIA_TRANSPORT_INPUT: u16 =
        uvc_if::descriptors::TerminalType::IttMediaTransportInput as u16;
    pub const OTT_VENDOR_SPECIFIC: u16 =
        uvc_if::descriptors::TerminalType::OttVendorSpecific as u16;
    pub const OTT_DISPLAY: u16 = uvc_if::descriptors::TerminalType::OttDisplay as u16;
    pub const OTT_MEDIA_TRANSPORT_OUTPUT: u16 =
        uvc_if::descriptors::TerminalType::OttMediaTransportOutput as u16;
    pub const EXTERNAL_VENDOR_SPECIFIC: u16 =
        uvc_if::descriptors::TerminalType::ExternalVendorSpecific as u16;
    pub const COMPOSITE_CONNECTOR: u16 =
        uvc_if::descriptors::TerminalType::CompositeConnector as u16;
    pub const SVIDEO_CONNECTOR: u16 = uvc_if::descriptors::TerminalType::SvideoConnector as u16;
    pub const COMPONENT_CONNECTOR: u16 =
        uvc_if::descriptors::TerminalType::ComponentConnector as u16;
}

pub mod descriptor_types {
    use usb_if::descriptor::DescriptorType;
    pub const DEVICE: u8 = DescriptorType::DEVICE.0;
    pub const CONFIGURATION: u8 = DescriptorType::CONFIGURATION.0;
    pub const STRING: u8 = DescriptorType::STRING.0;
    pub const INTERFACE: u8 = DescriptorType::INTERFACE.0;
    pub const ENDPOINT: u8 = DescriptorType::ENDPOINT.0;
    pub const CS_INTERFACE: u8 = DescriptorType::CLASS_SPECIFIC_INTERFACE.0;
    pub const CS_ENDPOINT: u8 = DescriptorType::CLASS_SPECIFIC_ENDPOINT.0;
}

pub mod format_guids {
    pub use uvc_if::descriptors::format_guids::{NV12, RGB24, UYVY, YUY2};
    // Preserve the direct driver's historical BGR24 name for the BGR3 GUID.
    pub const BGR24: [u8; 16] = uvc_if::descriptors::format_guids::BGR3;
}

pub mod payload_header_flags {
    use uvc_if::payload::PayloadHeaderFlags;
    pub const EOH: u8 = PayloadHeaderFlags::EOH.bits();
    pub const ERR: u8 = PayloadHeaderFlags::ERR.bits();
    pub const STI: u8 = PayloadHeaderFlags::STI.bits();
    pub const RES: u8 = PayloadHeaderFlags::RES.bits();
    pub const SCR: u8 = PayloadHeaderFlags::SCR.bits();
    pub const PTS: u8 = PayloadHeaderFlags::PTS.bits();
    pub const EOF: u8 = PayloadHeaderFlags::EOF.bits();
    pub const FID: u8 = PayloadHeaderFlags::FID.bits();
}

pub mod control_capabilities {
    use uvc_if::descriptors::ControlCapabilities;
    pub const GET: u8 = ControlCapabilities::GET.bits();
    pub const SET: u8 = ControlCapabilities::SET.bits();
    pub const DISABLED: u8 = ControlCapabilities::DISABLED.bits();
    pub const AUTOUPDATE: u8 = ControlCapabilities::AUTOUPDATE.bits();
    pub const ASYNCHRONOUS: u8 = ControlCapabilities::ASYNCHRONOUS.bits();
}
