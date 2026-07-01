"""LeetCode 9. Palindrome Number."""


def is_palindrome(x: int) -> bool:
    if x < 0:
        return False
    s = str(x)
    return s == s[::-1]


def main() -> None:
    cases = [(121, True), (-121, False), (10, False), (0, True), (12321, True)]
    for value, expected in cases:
        got = is_palindrome(value)
        status = "ok" if got == expected else "FAIL"
        print(f"is_palindrome({value}) = {got}  [{status}]")
        assert got == expected
    print("All Palindrome Number cases passed.")


if __name__ == "__main__":
    main()
