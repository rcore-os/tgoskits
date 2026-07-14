use core::mem::{offset_of, size_of};

use cvi_vdec_uapi::{
    CVI_VC_VDEC_CREATE_CHN, CVI_VC_VDEC_DESTROY_CHN, CVI_VC_VDEC_GET_FRAME,
    CVI_VC_VDEC_RELEASE_FRAME, CVI_VC_VDEC_SEND_STREAM, CVI_VC_VDEC_SET_CHN_PARAM,
    CVI_VC_VDEC_SET_JPEG_SCALE, CVI_VC_VDEC_START_RECV_STREAM, CVI_VC_VDEC_STOP_RECV_STREAM,
    VdecChnAttr, VdecChnParam, VdecChnStatus, VdecStream, VdecStreamEx, VideoFrame, VideoFrameInfo,
    VideoFrameInfoEx,
};

#[test]
fn ioctl_numbers_match_sg2002_linux() {
    assert_eq!(CVI_VC_VDEC_CREATE_CHN, 0x562f);
    assert_eq!(CVI_VC_VDEC_DESTROY_CHN, 0x5630);
    assert_eq!(CVI_VC_VDEC_START_RECV_STREAM, 0x5633);
    assert_eq!(CVI_VC_VDEC_STOP_RECV_STREAM, 0x5634);
    assert_eq!(CVI_VC_VDEC_SET_CHN_PARAM, 0x5637);
    assert_eq!(CVI_VC_VDEC_SEND_STREAM, 0x5639);
    assert_eq!(CVI_VC_VDEC_GET_FRAME, 0x563a);
    assert_eq!(CVI_VC_VDEC_RELEASE_FRAME, 0x563b);
    assert_eq!(CVI_VC_VDEC_SET_JPEG_SCALE, 0x5680);
}

#[test]
fn channel_and_stream_layouts_match_64_bit_sg2002_linux() {
    assert_eq!(size_of::<VdecChnAttr>(), 40);
    assert_eq!(offset_of!(VdecChnAttr, frame_buffer_count), 24);
    assert_eq!(offset_of!(VdecChnAttr, video_attr), 28);

    assert_eq!(size_of::<VdecChnParam>(), 32);
    assert_eq!(offset_of!(VdecChnParam, pixel_format), 4);
    assert_eq!(offset_of!(VdecChnParam, codec_param), 12);

    assert_eq!(size_of::<VdecStream>(), 32);
    assert_eq!(offset_of!(VdecStream, pts), 8);
    assert_eq!(offset_of!(VdecStream, end_of_frame), 16);
    assert_eq!(offset_of!(VdecStream, address), 24);
    assert_eq!(size_of::<VdecStreamEx>(), 16);
    assert_eq!(offset_of!(VdecStreamEx, timeout_ms), 8);

    assert_eq!(size_of::<VdecChnStatus>(), 72);
    assert_eq!(offset_of!(VdecChnStatus, receiving_stream), 16);
    assert_eq!(offset_of!(VdecChnStatus, received_stream_frames), 20);
    assert_eq!(offset_of!(VdecChnStatus, decode_error), 28);
    assert_eq!(offset_of!(VdecChnStatus, width), 64);
}

#[test]
fn frame_layout_matches_64_bit_sg2002_linux() {
    assert_eq!(size_of::<VideoFrame>(), 144);
    assert_eq!(offset_of!(VideoFrame, pixel_format), 8);
    assert_eq!(offset_of!(VideoFrame, stride), 32);
    assert_eq!(offset_of!(VideoFrame, physical_address), 48);
    assert_eq!(offset_of!(VideoFrame, virtual_address), 72);
    assert_eq!(offset_of!(VideoFrame, length), 96);
    assert_eq!(offset_of!(VideoFrame, offset_top), 108);
    assert_eq!(offset_of!(VideoFrame, time_ref), 116);
    assert_eq!(offset_of!(VideoFrame, pts), 120);
    assert_eq!(offset_of!(VideoFrame, private_data), 128);
    assert_eq!(offset_of!(VideoFrame, frame_flag), 136);

    assert_eq!(size_of::<VideoFrameInfo>(), 152);
    assert_eq!(offset_of!(VideoFrameInfo, pool_id), 144);
    assert_eq!(size_of::<VideoFrameInfoEx>(), 16);
    assert_eq!(offset_of!(VideoFrameInfoEx, timeout_ms), 8);
}
