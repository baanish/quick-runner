// LeetCode 412. Fizz Buzz
// Return the Fizz Buzz sequence from 1..=n.
function fizzBuzz(n) {
  const out = [];
  for (let i = 1; i <= n; i++) {
    if (i % 15 === 0) out.push("FizzBuzz");
    else if (i % 3 === 0) out.push("Fizz");
    else if (i % 5 === 0) out.push("Buzz");
    else out.push(String(i));
  }
  return out;
}

const got = fizzBuzz(15);
const expected = [
  "1", "2", "Fizz", "4", "Buzz", "Fizz", "7", "8",
  "Fizz", "Buzz", "11", "Fizz", "13", "14", "FizzBuzz",
];

console.log(got.join(" "));
const ok = JSON.stringify(got) === JSON.stringify(expected);
console.log(ok ? "Fizz Buzz case passed." : "Fizz Buzz case FAILED.");
if (!ok) process.exit(1);
