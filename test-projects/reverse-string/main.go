// LeetCode 344. Reverse String
package main

import "fmt"

func reverseString(s []byte) {
	for i, j := 0, len(s)-1; i < j; i, j = i+1, j-1 {
		s[i], s[j] = s[j], s[i]
	}
}

func main() {
	cases := []struct {
		in   string
		want string
	}{
		{"hello", "olleh"},
		{"Aa", "aA"},
		{"", ""},
		{"racecar", "racecar"},
	}

	allOK := true
	for _, c := range cases {
		b := []byte(c.in)
		reverseString(b)
		got := string(b)
		status := "ok"
		if got != c.want {
			status = "FAIL"
			allOK = false
		}
		fmt.Printf("reverseString(%q) = %q  [%s]\n", c.in, got, status)
	}
	if allOK {
		fmt.Println("All Reverse String cases passed.")
	}
}
