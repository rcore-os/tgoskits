use crate::Rtc;
use chrono::{DateTime, TimeZone as _, Utc};
use core::num::TryFromIntError;

impl Rtc {
    /// Returns the current time.
    pub fn get_time(&self) -> DateTime<Utc> {
        Utc.timestamp_opt(self.get_unix_timestamp().into(), 0)
            .unwrap()
    }

    /// Sets the current time.
    ///
    /// Returns an error if the given time is beyond the bounds supported by the RTC.
    pub fn set_time(&mut self, time: DateTime<Utc>) -> Result<(), TryFromIntError> {
        self.set_unix_timestamp(time.timestamp().try_into()?);
        Ok(())
    }

    /// Sets the match register to the given time. When the RTC value matches this then an interrupt
    /// will be generated (if it is enabled).
    pub fn set_match(&mut self, match_time: DateTime<Utc>) -> Result<(), TryFromIntError> {
        self.set_match_timestamp(match_time.timestamp().try_into()?);
        Ok(())
    }
}
