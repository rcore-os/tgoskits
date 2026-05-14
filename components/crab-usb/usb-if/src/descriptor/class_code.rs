/// USB Device Class Codes as defined by USB-IF
/// https://www.usb.org/defined-class-codes
#[derive(Debug, Clone, Copy)]
pub enum Class {
    /// Use class information in the Interface Descriptors
    ClassInInterface,
    /// Audio device
    Audio,
    /// Communications and CDC Control
    Communication,
    /// HID (Human Interface Device)
    Hid,
    /// Physical device
    Physical,
    /// Still Imaging device
    StillImaging,
    /// Printer device
    Printer,
    /// Mass Storage device
    MassStorage,
    /// Hub device
    Hub(HubSpeed),
    /// CDC-Data
    CdcData,
    /// Smart Card device
    SmartCard,
    /// Content Security device
    ContentSecurity,
    /// Video device
    Video,
    /// Personal Healthcare device
    PersonalHealthcare,
    /// Audio/Video Devices
    AudioVideo(AudioVideoType),
    /// Billboard Device
    Billboard,
    /// USB Type-C Bridge Device
    TypeCBridge,
    /// USB Bulk Display Protocol Device
    BulkDisplayProtocol,
    /// MCTP over USB Protocol Endpoint Device
    MctpOverUsb(MctpType),
    /// I3C Device
    I3c,
    /// Diagnostic Device
    Diagnostic(DiagnosticType),
    /// Wireless Controller
    Wireless(WirelessType),
    /// Miscellaneous
    Miscellaneous(MiscellaneousType),
    /// Application Specific
    Application(ApplicationType),
    /// Vendor Specific
    Vendor,
    /// Unknown/Other class codes
    Unknown {
        class: u8,
        subclass: u8,
        protocol: u8,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum HubSpeed {
    Full,
    HiSpeedSignalTT,
    HiSpeedMultipleTTs,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub enum AudioVideoType {
    AvControl,
    AvDataVideoStreaming,
    AvDataAudioStreaming,
}

#[derive(Debug, Clone, Copy)]
pub enum MctpType {
    ManagementControllerEndpoint(MctpVersion),
    HostInterfaceEndpoint(MctpVersion),
}

#[derive(Debug, Clone, Copy)]
pub enum MctpVersion {
    V1x,
    V2x,
}

#[derive(Debug, Clone, Copy)]
pub enum DiagnosticType {
    Usb2Compliance,
    DebugTarget(DebugProtocol),
    Trace(TraceProtocol),
    Dfx(DfxProtocol),
    Unknown(u8, u8), // (subclass, protocol)
}

#[derive(Debug, Clone, Copy)]
pub enum DebugProtocol {
    VendorDefined,
    GnuRemoteDebug,
}

#[derive(Debug, Clone, Copy)]
pub enum TraceProtocol {
    VendorDefined,
}

#[derive(Debug, Clone, Copy)]
pub enum DfxProtocol {
    VendorDefined,
}

#[derive(Debug, Clone, Copy)]
pub enum WirelessType {
    BluetoothProgramming,
    UwbRadioControl,
    RemoteNdis,
    BluetoothAmp,
    HostWireAdapter(WireAdapterInterface),
    DeviceWireAdapter(WireAdapterInterface),
}

#[derive(Debug, Clone, Copy)]
pub enum WireAdapterInterface {
    ControlData,
    Isochronous,
}

#[derive(Debug, Clone, Copy)]
pub enum MiscellaneousType {
    ActiveSync,
    PalmSync,
    InterfaceAssociation,
    WireAdapterMultifunction,
    CableBasedAssociation,
    Rndis(RndisType),
    Usb3Vision(VisionInterface),
    Step(StepType),
    DvbCi(DvbInterface),
}

#[derive(Debug, Clone, Copy)]
pub enum RndisType {
    Ethernet,
    Wifi,
    Wimax,
    Wwan,
    RawIpv4,
    RawIpv6,
    Gprs,
}

#[derive(Debug, Clone, Copy)]
pub enum VisionInterface {
    Control,
    Event,
    Streaming,
}

#[derive(Debug, Clone, Copy)]
pub enum StepType {
    Step,
    StepRaw,
}

#[derive(Debug, Clone, Copy)]
pub enum DvbInterface {
    CommandInIad,
    CommandInInterface,
    MediaInInterface,
}

#[derive(Debug, Clone, Copy)]
pub enum ApplicationType {
    DeviceFirmwareUpgrade,
    IrdaBridge,
    TestMeasurement(TestMeasurementType),
}

#[derive(Debug, Clone, Copy)]
pub enum TestMeasurementType {
    Standard,
    Usb488Subclass,
}

impl Class {
    pub fn from_class_and_subclass(class: u8, subclass: u8, protocol: u8) -> Self {
        match (class, subclass, protocol) {
            // Base Class 00h - Use class information in Interface Descriptors
            (0x00, 0x00, 0x00) => Self::ClassInInterface,

            // Base Class 01h - Audio
            (0x01, _, _) => Self::Audio,

            // Base Class 02h - Communications and CDC Control
            (0x02, _, _) => Self::Communication,

            // Base Class 03h - HID
            (0x03, _, _) => Self::Hid,

            // Base Class 05h - Physical
            (0x05, _, _) => Self::Physical,

            // Base Class 06h - Still Imaging
            (0x06, 0x01, 0x01) => Self::StillImaging,

            // Base Class 07h - Printer
            (0x07, _, _) => Self::Printer,

            // Base Class 08h - Mass Storage
            (0x08, _, _) => Self::MassStorage,

            // Base Class 09h - Hub
            (0x09, _, 0x00) => Self::Hub(HubSpeed::Full),
            (0x09, _, 0x01) => Self::Hub(HubSpeed::HiSpeedSignalTT),
            (0x09, _, 0x02) => Self::Hub(HubSpeed::HiSpeedMultipleTTs),
            (0x09, _, _) => Self::Hub(HubSpeed::Unknown),

            // Base Class 0Ah - CDC-Data
            (0x0A, _, _) => Self::CdcData,

            // Base Class 0Bh - Smart Card
            (0x0B, _, _) => Self::SmartCard,

            // Base Class 0Dh - Content Security
            (0x0D, _, _) => Self::ContentSecurity,

            // Base Class 0Eh - Video
            (0x0E, _, _) => Self::Video,

            // Base Class 0Fh - Personal Healthcare
            (0x0F, _, _) => Self::PersonalHealthcare,

            // Base Class 10h - Audio/Video Devices
            (0x10, 0x01, _) => Self::AudioVideo(AudioVideoType::AvControl),
            (0x10, 0x02, _) => Self::AudioVideo(AudioVideoType::AvDataVideoStreaming),
            (0x10, 0x03, _) => Self::AudioVideo(AudioVideoType::AvDataAudioStreaming),

            // Base Class 11h - Billboard Device
            (0x11, _, _) => Self::Billboard,

            // Base Class 12h - USB Type-C Bridge
            (0x12, _, _) => Self::TypeCBridge,

            // Base Class 13h - USB Bulk Display Protocol
            (0x13, _, _) => Self::BulkDisplayProtocol,

            // Base Class 14h - MCTP over USB
            (0x14, 0x00, 0x01) => {
                Self::MctpOverUsb(MctpType::ManagementControllerEndpoint(MctpVersion::V1x))
            }
            (0x14, 0x00, 0x02) => {
                Self::MctpOverUsb(MctpType::ManagementControllerEndpoint(MctpVersion::V2x))
            }
            (0x14, 0x01, 0x01) => {
                Self::MctpOverUsb(MctpType::HostInterfaceEndpoint(MctpVersion::V1x))
            }
            (0x14, 0x01, 0x02) => {
                Self::MctpOverUsb(MctpType::HostInterfaceEndpoint(MctpVersion::V2x))
            }

            // Base Class 3Ch - I3C Device
            (0x3C, _, _) => Self::I3c,

            // Base Class DCh - Diagnostic Device
            (0xDC, 0x01, 0x01) => Self::Diagnostic(DiagnosticType::Usb2Compliance),
            (0xDC, 0x02, 0x00) => {
                Self::Diagnostic(DiagnosticType::DebugTarget(DebugProtocol::VendorDefined))
            }
            (0xDC, 0x02, 0x01) => {
                Self::Diagnostic(DiagnosticType::DebugTarget(DebugProtocol::GnuRemoteDebug))
            }
            (0xDC, 0x03, 0x01) => {
                Self::Diagnostic(DiagnosticType::Trace(TraceProtocol::VendorDefined))
            }
            (0xDC, 0x04, 0x01) => Self::Diagnostic(DiagnosticType::Dfx(DfxProtocol::VendorDefined)),
            (0xDC, subclass, protocol) => {
                Self::Diagnostic(DiagnosticType::Unknown(subclass, protocol))
            }

            // Base Class E0h - Wireless Controller
            (0xE0, 0x01, 0x01) => Self::Wireless(WirelessType::BluetoothProgramming),
            (0xE0, 0x01, 0x02) => Self::Wireless(WirelessType::UwbRadioControl),
            (0xE0, 0x01, 0x03) => Self::Wireless(WirelessType::RemoteNdis),
            (0xE0, 0x01, 0x04) => Self::Wireless(WirelessType::BluetoothAmp),
            (0xE0, 0x02, 0x01) => Self::Wireless(WirelessType::HostWireAdapter(
                WireAdapterInterface::ControlData,
            )),
            (0xE0, 0x02, 0x02) => Self::Wireless(WirelessType::DeviceWireAdapter(
                WireAdapterInterface::ControlData,
            )),
            (0xE0, 0x02, 0x03) => Self::Wireless(WirelessType::DeviceWireAdapter(
                WireAdapterInterface::Isochronous,
            )),

            // Base Class EFh - Miscellaneous
            (0xEF, 0x01, 0x01) => Self::Miscellaneous(MiscellaneousType::ActiveSync),
            (0xEF, 0x01, 0x02) => Self::Miscellaneous(MiscellaneousType::PalmSync),
            (0xEF, 0x02, 0x01) => Self::Miscellaneous(MiscellaneousType::InterfaceAssociation),
            (0xEF, 0x02, 0x02) => Self::Miscellaneous(MiscellaneousType::WireAdapterMultifunction),
            (0xEF, 0x03, 0x01) => Self::Miscellaneous(MiscellaneousType::CableBasedAssociation),
            (0xEF, 0x04, 0x01) => {
                Self::Miscellaneous(MiscellaneousType::Rndis(RndisType::Ethernet))
            }
            (0xEF, 0x04, 0x02) => Self::Miscellaneous(MiscellaneousType::Rndis(RndisType::Wifi)),
            (0xEF, 0x04, 0x03) => Self::Miscellaneous(MiscellaneousType::Rndis(RndisType::Wimax)),
            (0xEF, 0x04, 0x04) => Self::Miscellaneous(MiscellaneousType::Rndis(RndisType::Wwan)),
            (0xEF, 0x04, 0x05) => Self::Miscellaneous(MiscellaneousType::Rndis(RndisType::RawIpv4)),
            (0xEF, 0x04, 0x06) => Self::Miscellaneous(MiscellaneousType::Rndis(RndisType::RawIpv6)),
            (0xEF, 0x04, 0x07) => Self::Miscellaneous(MiscellaneousType::Rndis(RndisType::Gprs)),
            (0xEF, 0x05, 0x00) => {
                Self::Miscellaneous(MiscellaneousType::Usb3Vision(VisionInterface::Control))
            }
            (0xEF, 0x05, 0x01) => {
                Self::Miscellaneous(MiscellaneousType::Usb3Vision(VisionInterface::Event))
            }
            (0xEF, 0x05, 0x02) => {
                Self::Miscellaneous(MiscellaneousType::Usb3Vision(VisionInterface::Streaming))
            }
            (0xEF, 0x06, 0x01) => Self::Miscellaneous(MiscellaneousType::Step(StepType::Step)),
            (0xEF, 0x06, 0x02) => Self::Miscellaneous(MiscellaneousType::Step(StepType::StepRaw)),
            (0xEF, 0x07, 0x01) => {
                Self::Miscellaneous(MiscellaneousType::DvbCi(DvbInterface::CommandInIad))
            }
            (0xEF, 0x07, 0x02) => {
                Self::Miscellaneous(MiscellaneousType::DvbCi(DvbInterface::CommandInInterface))
            }
            (0xEF, 0x07, 0x03) => {
                Self::Miscellaneous(MiscellaneousType::DvbCi(DvbInterface::MediaInInterface))
            }

            // Base Class FEh - Application Specific
            (0xFE, 0x01, 0x01) => Self::Application(ApplicationType::DeviceFirmwareUpgrade),
            (0xFE, 0x02, 0x00) => Self::Application(ApplicationType::IrdaBridge),
            (0xFE, 0x03, 0x00) => Self::Application(ApplicationType::TestMeasurement(
                TestMeasurementType::Standard,
            )),
            (0xFE, 0x03, 0x01) => Self::Application(ApplicationType::TestMeasurement(
                TestMeasurementType::Usb488Subclass,
            )),

            // Base Class FFh - Vendor Specific
            (0xFF, _, _) => Self::Vendor,

            _ => Self::Unknown {
                class,
                subclass,
                protocol,
            },
        }
    }
}
