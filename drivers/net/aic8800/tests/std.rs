use aic8800::{
    AicAction, AicDevice, AicInput, AicInputEvent, ChipVariant, MonotonicTime, SdioCompletion,
    SdioRequestKind, SdioResponse,
};

#[test]
fn public_startup_api_owns_the_sdio_function_lifecycle() {
    let now = MonotonicTime::from_nanos(0);
    let mut device = AicDevice::new(ChipVariant::Aic8801).expect("AIC8801 is supported");
    device.start(now).expect("stopped device can start");

    let AicAction::SubmitSdio(enable) = device.advance(AicInput::tick(now)) else {
        panic!("startup must request SDIO function enable")
    };
    assert!(matches!(
        enable.kind,
        SdioRequestKind::EnableFunction(function) if function.get() == 1
    ));

    let action = device.advance(AicInput {
        now,
        event: Some(AicInputEvent::Sdio(SdioCompletion {
            request_id: enable.id,
            result: Ok(SdioResponse::Unit),
        })),
    });
    assert!(matches!(
        action,
        AicAction::SubmitSdio(request)
            if matches!(request.kind, SdioRequestKind::SetBlockSize { .. })
    ));
}
