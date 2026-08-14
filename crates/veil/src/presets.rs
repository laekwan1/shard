//! Curated domain lists for routing.
//!
//! Typing thirty banking domains by hand is the kind of setup step people skip,
//! and skipping it is what gets an account locked. These ship with the app so
//! it is one click instead.
//!
//! Every entry is a suffix, so `kbstar.com` also covers `obank.kbstar.com`.

/// Korean financial and government services.
///
/// These must bypass the tunnel. Banks, brokerages and government portals treat
/// a foreign address as fraud and will lock the account or refuse the session —
/// and certificate-based authentication frequently fails outright.
pub const KOREAN_FINANCE: &[&str] = &[
    // 은행
    "kbstar.com",
    "shinhan.com",
    "shinhanbank.com",
    "wooribank.com",
    "hanabank.com",
    "kebhana.com",
    "nonghyup.com",
    "nhbank.com",
    "ibk.co.kr",
    "kakaobank.com",
    "tossbank.com",
    "toss.im",
    "kbanknow.com",
    "standardchartered.co.kr",
    "citibank.co.kr",
    "kfcc.co.kr",
    "cu.co.kr",
    "suhyup-bank.com",
    "busanbank.co.kr",
    "knbank.co.kr",
    "kjbank.com",
    "jbbank.co.kr",
    "dgb.co.kr",
    // 카드
    "samsungcard.com",
    "hyundaicard.com",
    "lottecard.co.kr",
    "bccard.com",
    "shinhancard.com",
    "wooricard.com",
    "hanacard.com",
    // 증권
    "miraeasset.com",
    "samsungpop.com",
    "kiwoom.com",
    "nhqv.com",
    "truefriend.com",
    "daishin.com",
    "ebestsec.co.kr",
    "shinhaninvest.com",
    // 결제 · 인증
    "kakaopay.com",
    "payco.com",
    "yessign.or.kr",
    "signgate.com",
    "crosscert.com",
    // 정부 — go.kr 하나로 모든 정부 사이트를 덮습니다
    "go.kr",
    "gov.kr",
];

/// Large domestic services. Not a correctness issue like finance — routing
/// these through a foreign server just makes them slower for no benefit, and
/// some geo-restrict.
pub const KOREAN_DOMESTIC: &[&str] = &[
    "naver.com",
    "naver.net",
    "daum.net",
    "kakao.com",
    "kakaocdn.net",
    "coupang.com",
    "11st.co.kr",
    "gmarket.co.kr",
    "melon.com",
    "tving.com",
    "wavve.com",
    "netmarble.net",
    "nexon.com",
    "ncsoft.com",
];

/// Add every entry that is not already present, preserving what the user has.
/// Returns how many were newly added.
pub fn merge_into(list: &mut Vec<String>, preset: &[&str]) -> usize {
    let mut added = 0;
    for entry in preset {
        if !list.iter().any(|existing| existing.eq_ignore_ascii_case(entry)) {
            list.push((*entry).to_string());
            added += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_skips_what_is_already_there() {
        let mut list = vec!["kbstar.com".to_string(), "mine.example".to_string()];
        let added = merge_into(&mut list, KOREAN_FINANCE);

        assert_eq!(added, KOREAN_FINANCE.len() - 1, "kbstar.com was already present");
        assert_eq!(list.iter().filter(|d| *d == "kbstar.com").count(), 1);
        assert!(list.contains(&"mine.example".to_string()), "user entries must survive");
    }

    #[test]
    fn merging_twice_changes_nothing_the_second_time() {
        let mut list = Vec::new();
        let first = merge_into(&mut list, KOREAN_FINANCE);
        let second = merge_into(&mut list, KOREAN_FINANCE);
        assert_eq!(first, KOREAN_FINANCE.len());
        assert_eq!(second, 0);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let mut list = vec!["KBSTAR.COM".to_string()];
        merge_into(&mut list, &["kbstar.com"]);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn presets_have_no_duplicates_and_no_bare_tlds() {
        for preset in [KOREAN_FINANCE, KOREAN_DOMESTIC] {
            let mut seen: Vec<&str> = preset.to_vec();
            let count = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), count, "duplicate entry in preset");

            for entry in preset {
                assert!(entry.contains('.'), "{entry} is not a domain");
                // A bare co.kr or or.kr would divert far more than intended.
                assert!(!matches!(*entry, "co.kr" | "or.kr" | "kr" | "com"), "{entry} is too broad");
            }
        }
    }

    #[test]
    fn government_is_covered_by_one_suffix() {
        // Suffix matching means go.kr reaches every government site, so listing
        // them individually would be noise.
        assert!(KOREAN_FINANCE.contains(&"go.kr"));
        assert!(!KOREAN_FINANCE.contains(&"hometax.go.kr"));
    }
}
