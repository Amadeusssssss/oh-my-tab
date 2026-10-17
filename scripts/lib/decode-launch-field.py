#!/usr/bin/env python3
"""Decode one field of the app's launch record.

The inverse of escape_field in src/logger.rs. It lives in a file of its own so the shell pipeline
does not have to carry the escaping rules, and so the codec can be tested against the same code the
script actually runs.
"""

import sys

# One entry per escape the encoder emits. Anything else after a backslash is taken literally, so a
# value the encoder never produced is not silently dropped.
TABLE = {
    "s": " ",
    "e": "=",
    "o": "[",
    "c": "]",
    "n": "\n",
    "r": "\r",
    "\\": "\\",
}


def decode(value):
    if value == "-":
        return ""
    out, index = [], 0
    while index < len(value):
        char = value[index]
        if char == "\\" and index + 1 < len(value):
            following = value[index + 1]
            if following in TABLE:
                out.append(TABLE[following])
            else:
                # Not an escape the encoder emits, so it cannot be a field value either: keep it as
                # written rather than dropping the backslash and inventing a different path.
                out.append(char)
                out.append(following)
            index += 2
        else:
            out.append(char)
            index += 1
    return "".join(out)


if __name__ == "__main__":
    sys.stdout.write(decode(sys.stdin.read()))
