//! Destinations that must not go through the tunnel.
//!
//! This is not a performance list. Korean banks, brokerages and government
//! portals treat a foreign exit address as fraud: the session is refused, and
//! certificate-based login fails outright. Sending them through the tunnel
//! would not make them private, it would make them stop working — and the
//! failure looks like the site being down rather than like a setting.

/// Finance and government. Correctness, not preference.
pub const KOREAN_ESSENTIAL: &[&str] = &[
    // 은행
    "kbstar.com", "shinhan.com", "shinhansec.com", "wooribank.com", "hanabank.com",
    "kebhana.com", "nonghyup.com", "nhbank.com", "ibk.co.kr", "kdb.co.kr",
    "citibank.co.kr", "standardchartered.co.kr", "kfcc.co.kr", "cu.co.kr",
    "epostbank.go.kr", "kakaobank.com", "kbanknow.com", "tossbank.com",
    // 증권 · 카드
    "miraeasset.com", "samsungpop.com", "kiwoom.com", "koreainvestment.com",
    "ebestsec.co.kr", "daishin.com", "hanaw.com", "truefriend.com",
    "samsungcard.com", "shinhancard.com", "hyundaicard.com", "lottecard.co.kr",
    "bccard.com", "kbcard.com", "hanacard.co.kr",
    // 인증 · 결제
    "yessign.or.kr", "crosscert.com", "signkorea.com", "tradesign.net",
    "kftc.or.kr", "koreanbank.or.kr", "inicis.com", "kcp.co.kr", "nicepay.co.kr",
    "danal.co.kr", "settlebank.co.kr", "tosspayments.com",
    // 정부 · 공공
    "go.kr", "or.kr", "re.kr", "ac.kr",
];

/// Large domestic services. Routing these abroad only makes them slower, and
/// some refuse a foreign address outright.
pub const KOREAN_DOMESTIC: &[&str] = &[
    "naver.com", "naver.net", "pstatic.net", "daum.net", "kakao.com", "kakaocdn.net",
    "coupang.com", "coupangcdn.com", "11st.co.kr", "gmarket.co.kr", "auction.co.kr",
    "melon.com", "genie.co.kr", "bugs.co.kr", "tving.com", "wavve.com",
    "netflix.com", "watcha.com", "afreecatv.com", "chzzk.naver.com",
    "toss.im", "kakaopay.com", "baemin.com", "yogiyo.co.kr",
];

/// Everything that should leave the phone directly.
pub fn korean_direct() -> Vec<String> {
    KOREAN_ESSENTIAL
        .iter()
        .chain(KOREAN_DOMESTIC)
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::DirectRules;

    #[test]
    fn the_banks_that_matter_are_covered() {
        let rules = DirectRules::new(korean_direct());
        for host in [
            "obank.kbstar.com",
            "www.shinhan.com",
            "banking.nonghyup.com",
            "www.hometax.go.kr",
            "cert.yessign.or.kr",
        ] {
            assert!(rules.applies_to(host), "{host} would have gone through the tunnel");
        }
    }

    #[test]
    fn a_site_worth_tunnelling_is_not_on_the_list() {
        let rules = DirectRules::new(korean_direct());
        for host in ["xvideos.com", "www.pornhub.com", "example.com", "youtube.com"] {
            assert!(!rules.applies_to(host), "{host} would have bypassed the tunnel");
        }
    }

    #[test]
    fn no_entry_is_listed_twice() {
        // A duplicate is harmless but means the list was edited carelessly, and
        // the next edit is the one that removes the wrong copy.
        let all = korean_direct();
        let mut seen = std::collections::HashSet::new();
        for entry in &all {
            assert!(seen.insert(entry.clone()), "{entry} is listed twice");
        }
    }
}
