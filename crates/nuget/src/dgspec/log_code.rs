//! NuGet's warning and error codes, as a dgspec's warning lists name them.
//!
//! The reader keeps a listed code only if `Enum.TryParse<NuGetLogCode>` accepts
//! it, and that accepts more than the names: `1605` and ` NU1605` both read as
//! `NU1605`, and `NU1605,NU1603` reads as the meaningless value 1607. So a code
//! is accepted here only when it is spelled exactly as one of the names below,
//! and anything else declines. The list is `NuGetLogCode`'s names (less
//! `Undefined`) in the NuGet that `tests/all/dgspec_diff.rs` grades against;
//! that test fails if the two lists differ, since a name the reader does not
//! know is one it drops.

use std::fmt;

/// One `NuGetLogCode` name, such as `NU1605`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NuGetLogCode(&'static str);

impl NuGetLogCode {
    /// The code spelled exactly as `text`, or `None` if `text` is not one of the
    /// names (see the module docs for why near-misses are not accepted).
    pub fn parse(text: &str) -> Option<NuGetLogCode> {
        NAMES
            .binary_search(&text)
            .ok()
            .map(|index| NuGetLogCode(NAMES[index]))
    }

    pub fn as_str(&self) -> &'static str {
        self.0
    }

    /// Every name, in ascending order.
    pub fn all() -> impl Iterator<Item = NuGetLogCode> {
        NAMES.iter().map(|name| NuGetLogCode(name))
    }
}

impl fmt::Display for NuGetLogCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Sorted, so [`NuGetLogCode::parse`] can binary-search it.
const NAMES: &[&str] = &[
    "NU1000", "NU1001", "NU1002", "NU1003", "NU1004", "NU1005", "NU1006", "NU1007", "NU1008",
    "NU1009", "NU1010", "NU1011", "NU1012", "NU1013", "NU1014", "NU1015", "NU1016", "NU1017",
    "NU1018", "NU1019", "NU1100", "NU1101", "NU1102", "NU1103", "NU1104", "NU1105", "NU1106",
    "NU1107", "NU1108", "NU1109", "NU1110", "NU1201", "NU1202", "NU1203", "NU1204", "NU1211",
    "NU1212", "NU1213", "NU1301", "NU1302", "NU1401", "NU1402", "NU1403", "NU1410", "NU1500",
    "NU1501", "NU1502", "NU1503", "NU1504", "NU1505", "NU1506", "NU1507", "NU1508", "NU1509",
    "NU1510", "NU1511", "NU1512", "NU1601", "NU1602", "NU1603", "NU1604", "NU1605", "NU1608",
    "NU1701", "NU1702", "NU1703", "NU1801", "NU1802", "NU1803", "NU1900", "NU1901", "NU1902",
    "NU1903", "NU1904", "NU1905", "NU3000", "NU3001", "NU3002", "NU3003", "NU3004", "NU3005",
    "NU3006", "NU3007", "NU3008", "NU3009", "NU3010", "NU3011", "NU3012", "NU3013", "NU3014",
    "NU3015", "NU3016", "NU3017", "NU3018", "NU3019", "NU3020", "NU3021", "NU3022", "NU3023",
    "NU3024", "NU3025", "NU3026", "NU3027", "NU3028", "NU3029", "NU3030", "NU3031", "NU3032",
    "NU3033", "NU3034", "NU3035", "NU3036", "NU3037", "NU3038", "NU3039", "NU3040", "NU3041",
    "NU3042", "NU3043", "NU5000", "NU5001", "NU5002", "NU5003", "NU5004", "NU5005", "NU5007",
    "NU5008", "NU5009", "NU5010", "NU5011", "NU5012", "NU5013", "NU5014", "NU5015", "NU5016",
    "NU5017", "NU5018", "NU5019", "NU5020", "NU5021", "NU5022", "NU5023", "NU5024", "NU5025",
    "NU5026", "NU5027", "NU5028", "NU5029", "NU5030", "NU5031", "NU5032", "NU5033", "NU5034",
    "NU5035", "NU5036", "NU5037", "NU5038", "NU5039", "NU5040", "NU5041", "NU5042", "NU5045",
    "NU5046", "NU5047", "NU5048", "NU5049", "NU5050", "NU5051", "NU5052", "NU5100", "NU5101",
    "NU5102", "NU5103", "NU5104", "NU5105", "NU5106", "NU5107", "NU5108", "NU5109", "NU5110",
    "NU5111", "NU5112", "NU5114", "NU5115", "NU5116", "NU5117", "NU5118", "NU5119", "NU5120",
    "NU5121", "NU5122", "NU5123", "NU5124", "NU5125", "NU5126", "NU5127", "NU5128", "NU5129",
    "NU5130", "NU5131", "NU5132", "NU5133", "NU5500", "NU5501", "NU5502",
];

#[cfg(test)]
mod tests {
    use super::NAMES;

    #[test]
    fn names_are_sorted_and_distinct() {
        assert!(NAMES.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
