use loongArch64::register::{ecfg, estat::Estat};

pub(super) fn is_spurious_interrupt(estat: &Estat) -> bool {
    estat.ecode() == 0 && estat.is() == 0 && ecfg::read().vs() == 0
}
