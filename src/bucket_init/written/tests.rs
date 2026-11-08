use super::*;

#[test]
fn every_member_has_its_own_bit_and_recorded_sets_decode_back() {
    let mut seen = 0;
    for member in Member::ALL {
        assert_eq!(seen & member.bit(), 0, "{member:?} shares a bit");
        seen |= member.bit();
    }
    let all: Written = Member::ALL.iter().copied().collect();
    assert_eq!(all.bits(), MemberBits::new(seen));
    assert!(Written::default().removed_since(all.bits()).len() == Member::ALL.len());
    let some: Written = [Member::Cors, Member::IamPolicy].into_iter().collect();
    assert_eq!(
        some.removed_since(all.bits()),
        Member::ALL
            .iter()
            .copied()
            .filter(|member| !matches!(member, Member::Cors | Member::IamPolicy))
            .collect::<Vec<_>>()
    );
    assert!(all.removed_since(some.bits()).is_empty());
    // Bits that no member uses mean nothing rather than failing.
    assert!(all.removed_since(MemberBits::new(u32::MAX)).is_empty());
}
