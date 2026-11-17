//! Asana's numbers as [`Decimal`], read from and written to JSON numbers.
//!
//! Asana's `number` values (custom field numbers, rates, budgets, time in minutes) are held here
//! as `rust_decimal::Decimal`, so arithmetic on money never meets binary rounding. On the wire they
//! are plain JSON numbers, and Asana itself stores them as doubles, so about 15 significant digits
//! survive a round trip through Asana whatever a client does.
//!
//! This crate does not switch on `serde_json`'s `arbitrary_precision`: Cargo unifies features
//! across a whole build, so a library that enabled it would change how every JSON number is held
//! in every application that used the library. Instead:
//!
//! - **Reading.** A number is read as the shortest text that turns back into the same double (Rust's
//!   `Display` for `f64`) and parsed as a decimal, so `1250.5` is `1250.5`, never
//!   `1250.4999999999999`. An integer is read exactly. A number given as a string is accepted too.
//! - **Writing.** An integer that fits `i64` or `u64` is written as an integer. Anything else is
//!   written as the double nearest to it, which prints the decimal's own digits whenever a double
//!   can carry them ([`fits_a_double`] says whether it can). Trailing zeros are not part of a
//!   JSON number: `1250.50` is written `1250.5`.

use std::fmt;
use std::str::FromStr;

use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::de::{self, Deserializer, Visitor};
use serde::ser::Serializer;

/// How a decimal goes out as a JSON number.
enum Wire {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
}

fn wire(value: Decimal) -> Wire {
    let normal = value.normalize();
    if normal.scale() == 0 {
        if let Some(signed) = normal.to_i64() {
            return Wire::Signed(signed);
        }
        if let Some(unsigned) = normal.to_u64() {
            return Wire::Unsigned(unsigned);
        }
    }
    // A decimal's text always parses as a double, rounded to the nearest one; `to_f64` is the
    // fallback that cannot arise.
    let float = f64::from_str(&normal.to_string())
        .ok()
        .or_else(|| normal.to_f64())
        .unwrap_or(0.0);
    Wire::Float(float)
}

/// The JSON number a decimal is written as: an integer when it is one that fits `i64` or `u64`,
/// otherwise the nearest double.
pub fn to_json_number(value: Decimal) -> serde_json::Number {
    match wire(value) {
        Wire::Signed(signed) => signed.into(),
        Wire::Unsigned(unsigned) => unsigned.into(),
        // A decimal is always finite, so this is always `Some`; zero is the fallback that cannot
        // arise.
        Wire::Float(float) => serde_json::Number::from_f64(float).unwrap_or_else(|| 0.into()),
    }
}

/// Whether the decimal goes out with all its digits: an integer that fits `i64` or `u64`, or a
/// value whose nearest double prints the same digits. `false` means Asana receives the nearest
/// double instead (for example for more than about 15 significant digits).
pub fn fits_a_double(value: Decimal) -> bool {
    let normal = value.normalize();
    match wire(normal) {
        Wire::Signed(_) | Wire::Unsigned(_) => true,
        Wire::Float(float) => decimal_from_f64(float) == Some(normal),
    }
}

/// The decimal a double stands for: its shortest round-trip text, parsed. `None` for a value no
/// decimal can hold (not finite, or beyond about 7.9 × 10²⁸).
pub fn decimal_from_f64(value: f64) -> Option<Decimal> {
    if !value.is_finite() {
        return None;
    }
    // `Display` never uses an exponent, so the text is plain digits; a value with more than 28
    // decimal places is rounded to fit by `from_f64`.
    Decimal::from_str(&value.to_string())
        .ok()
        .or_else(|| Decimal::from_scientific(&format!("{value:e}")).ok())
        .or_else(|| Decimal::from_f64(value))
        .map(|decimal| decimal.normalize())
}

/// Parses a number Asana sent as text, plain (`1250.5`) or with an exponent (`1e21`).
fn decimal_from_text(text: &str) -> Option<Decimal> {
    let text = text.trim();
    Decimal::from_str(text)
        .ok()
        .or_else(|| Decimal::from_scientific(text).ok())
}

/// Writes a decimal as a JSON number, as [`to_json_number`] describes.
///
/// # Errors
///
/// The serializer's own error.
pub fn serialize<S: Serializer>(value: &Decimal, serializer: S) -> Result<S::Ok, S::Error> {
    match wire(*value) {
        Wire::Signed(signed) => serializer.serialize_i64(signed),
        Wire::Unsigned(unsigned) => serializer.serialize_u64(unsigned),
        Wire::Float(float) => serializer.serialize_f64(float),
    }
}

/// Reads a decimal from a JSON number (or a number in a string), as the module describes.
///
/// # Errors
///
/// The deserializer's error when the value is not a number or no decimal can hold it.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Decimal, D::Error> {
    deserializer.deserialize_any(DecimalVisitor)
}

struct DecimalVisitor;

impl Visitor<'_> for DecimalVisitor {
    type Value = Decimal;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a number")
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Decimal, E> {
        Ok(Decimal::from(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Decimal, E> {
        Ok(Decimal::from(value))
    }

    fn visit_i128<E: de::Error>(self, value: i128) -> Result<Decimal, E> {
        Decimal::from_i128(value).ok_or_else(|| E::custom("a number too large for a decimal"))
    }

    fn visit_u128<E: de::Error>(self, value: u128) -> Result<Decimal, E> {
        Decimal::from_u128(value).ok_or_else(|| E::custom("a number too large for a decimal"))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Decimal, E> {
        decimal_from_f64(value).ok_or_else(|| E::custom("a number no decimal can hold"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Decimal, E> {
        decimal_from_text(value).ok_or_else(|| E::custom("text that is not a decimal number"))
    }
}

/// `Option<Decimal>` as an optional JSON number, for `#[serde(with = "...")]`: `null` or absent
/// is `None`.
pub mod option {
    use std::fmt;

    use rust_decimal::Decimal;
    use serde::de::{self, Deserializer, Visitor};
    use serde::ser::Serializer;

    /// Writes `Some` as a JSON number and `None` as `null`.
    ///
    /// # Errors
    ///
    /// The serializer's own error.
    pub fn serialize<S: Serializer>(
        value: &Option<Decimal>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(decimal) => super::serialize(decimal, serializer),
            None => serializer.serialize_none(),
        }
    }

    /// Reads `null` as `None` and a number as `Some`.
    ///
    /// # Errors
    ///
    /// The deserializer's error when the value is neither `null` nor a number a decimal can hold.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Decimal>, D::Error> {
        deserializer.deserialize_option(OptionVisitor)
    }

    struct OptionVisitor;

    impl<'de> Visitor<'de> for OptionVisitor {
        type Value = Option<Decimal>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a number or null")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            super::deserialize(deserializer).map(Some)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimal(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    #[test]
    fn a_number_is_read_through_its_shortest_text() {
        let cases = [
            ("1250.5", "1250.5"),
            ("0.1", "0.1"),
            ("1e21", "1000000000000000000000"),
            ("1.5e-7", "0.00000015"),
            ("-0.0", "0"),
            ("1234567890.123456789", "1234567890.1234567"),
        ];
        for (json, expected) in cases {
            let read: Option<Decimal> =
                option::deserialize(&mut serde_json::Deserializer::from_str(json)).unwrap();
            assert_eq!(read, Some(decimal(expected)), "{json}");
        }
    }

    #[test]
    fn integers_strings_and_null_are_read() {
        let read = |json: &str| {
            option::deserialize(&mut serde_json::Deserializer::from_str(json)).unwrap()
        };
        assert_eq!(read("18446744073709551615"), Some(Decimal::from(u64::MAX)));
        assert_eq!(read("-42"), Some(Decimal::from(-42)));
        assert_eq!(read("\"12.50\""), Some(decimal("12.50")));
        assert_eq!(read("\"2e3\""), Some(decimal("2000")));
        assert_eq!(read("null"), None);
        assert!(
            option::deserialize(&mut serde_json::Deserializer::from_str("1e300")).is_err(),
            "beyond what a decimal holds"
        );
    }

    #[test]
    fn a_decimal_is_written_as_an_integer_or_its_nearest_double() {
        let cases = [
            ("1250.50", "1250.5", true),
            ("1250.00", "1250", true),
            ("-0.5", "-0.5", true),
            ("18446744073709551615", "18446744073709551615", true),
            ("0.0000000000000000000000000001", "1e-28", true),
            ("1234567890.123456789", "1234567890.1234567", false),
            (
                "79228162514264337593543950335",
                "7.922816251426434e+28",
                false,
            ),
        ];
        for (text, written, exact) in cases {
            let value = decimal(text);
            let number = to_json_number(value);
            assert_eq!(number.to_string(), written, "{text}");
            assert_eq!(
                serde_json::to_string(&serde_json::Value::Number(number)).unwrap(),
                written
            );
            assert_eq!(fits_a_double(value), exact, "{text}");
        }
    }
}
