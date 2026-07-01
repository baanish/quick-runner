use std::collections::HashMap;

/// LeetCode 1. Two Sum
/// Return indices of the two numbers that add up to `target`.
fn two_sum(nums: &[i32], target: i32) -> Option<(usize, usize)> {
    let mut seen: HashMap<i32, usize> = HashMap::new();
    for (i, &n) in nums.iter().enumerate() {
        if let Some(&j) = seen.get(&(target - n)) {
            return Some((j, i));
        }
        seen.insert(n, i);
    }
    None
}

fn main() {
    let cases = [
        (vec![2, 7, 11, 15], 9, Some((0, 1))),
        (vec![3, 2, 4], 6, Some((1, 2))),
        (vec![3, 3], 6, Some((0, 1))),
        (vec![1, 2, 3], 100, None),
    ];

    for (nums, target, expected) in cases {
        let got = two_sum(&nums, target);
        let status = if got == expected { "ok" } else { "FAIL" };
        println!("two_sum({nums:?}, {target}) = {got:?}  [{status}]");
        assert_eq!(got, expected);
    }
    println!("All Two Sum cases passed.");
}
