use std::{fmt, str::FromStr};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl TimeRange {
    pub fn start(self) -> f64 {
        self.start_ms as f64 / 1000.0
    }

    pub fn end(self) -> f64 {
        self.end_ms as f64 / 1000.0
    }

    pub fn suffix(self) -> String {
        format!("-t{}-{}ms", self.start_ms, self.end_ms)
    }
}

impl fmt::Display for TimeRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:.3}-{:.3}] 秒", self.start(), self.end())
    }
}

impl FromStr for TimeRange {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim();
        let value = if value.starts_with('[') && value.ends_with(']') {
            &value[1..value.len() - 1]
        } else {
            if value.contains(['[', ']']) {
                bail!("时间区间的方括号必须成对出现");
            }
            value
        };
        let (start, end) = value
            .split_once('-')
            .context("时间区间格式应为 [HH:MM:SS-HH:MM:SS]、[MM:SS-MM:SS] 或 [秒-秒]")?;
        let start_ms = timestamp_ms(start.trim())?;
        let end_ms = timestamp_ms(end.trim())?;
        if start_ms >= end_ms {
            bail!("时间区间必须满足 0 <= 起点 < 终点");
        }
        Ok(Self { start_ms, end_ms })
    }
}

fn timestamp_ms(value: &str) -> Result<u64> {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 3 {
        bail!("无法识别时间 `{value}`");
    }
    let mut millis = 0u64;
    for (index, part) in parts.iter().enumerate() {
        let last = index == parts.len() - 1;
        let (whole, fraction) = part.split_once('.').unwrap_or((part, ""));
        if whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || (!fraction.is_empty() && (!last || fraction.len() > 3))
            || !fraction.bytes().all(|b| b.is_ascii_digit())
            || (part.contains('.') && fraction.is_empty())
        {
            bail!("时间 `{value}` 不合法；秒数最多支持三位小数");
        }
        let number: u64 = whole.parse().context("时间数值过大")?;
        if index > 0 && number >= 60 {
            bail!("时间 `{value}` 中的秒或小时后的分钟必须小于 60");
        }
        millis = millis
            .checked_mul(60)
            .and_then(|v| number.checked_mul(1000).and_then(|n| v.checked_add(n)))
            .context("时间数值过大")?;
        if !fraction.is_empty() {
            let fraction = fraction.parse::<u64>()? * 10u64.pow(3 - fraction.len() as u32);
            millis = millis.checked_add(fraction).context("时间数值过大")?;
        }
    }
    Ok(millis)
}

#[cfg(test)]
mod tests {
    use super::TimeRange;

    #[test]
    fn accepts_equivalent_formats_and_fractional_seconds() {
        for value in ["[01:02:03-01:02:04.5]", "[62:03-62:04.500]", "3723-3724.5"] {
            assert_eq!(
                value.parse::<TimeRange>().unwrap(),
                TimeRange {
                    start_ms: 3723000,
                    end_ms: 3724500
                }
            );
        }
    }

    #[test]
    fn rejects_invalid_ranges_and_overflow() {
        for value in [
            "",
            "[1-2",
            "1-2]",
            "[-1-2]",
            "2-2",
            "3-2",
            "NaN-10",
            "0-inf",
            "0-1e3",
            "00:60-2:00",
            "1:60:00-2:00:00",
            "1.5:00-2:00",
            "1-2-3",
            "1.-2",
            "0-0.0001",
            "0-18446744073709551615",
        ] {
            assert!(value.parse::<TimeRange>().is_err(), "{value}");
        }
    }
}
