package tools

import (
	"fmt"
	"regexp"
	"strings"
)

// compileGlob compiles a slash-separated glob pattern into an anchored
// regexp with the old stack's globset settings (search.rs:313-321):
// backslashes normalize to '/', '*' and '?' never cross a directory
// separator (literal_separator), a whole '**' segment matches zero or more
// segments, and character classes support [abc], [a-z] and negation
// [!abc]. An unterminated '[' is an error like globset's unclosed character
// class. Brace expansion is not supported in v1 (a divergence: '{a,b}'
// matches literally).
func compileGlob(pattern string) (*regexp.Regexp, error) {
	normalized := strings.ReplaceAll(pattern, "\\", "/")
	segments := strings.Split(normalized, "/")
	var out strings.Builder
	out.WriteString("^")
	for i, segment := range segments {
		last := i == len(segments)-1
		if segment == "**" {
			if last {
				// A trailing ** matches everything below this point, at
				// least one segment deep.
				out.WriteString("(?:[^/]+/)*[^/]+")
			} else {
				// A non-final ** consumes its own separator and any number
				// of whole segments.
				out.WriteString("(?:[^/]+/)*")
				continue
			}
		} else {
			body, err := globSegmentExpr(segment)
			if err != nil {
				return nil, err
			}
			out.WriteString(body)
		}
		if !last {
			out.WriteString("/")
		}
	}
	out.WriteString("$")
	expr, err := regexp.Compile(out.String())
	if err != nil {
		return nil, fmt.Errorf("Glob 无效：%v", err)
	}
	return expr, nil
}

// globSegmentExpr translates one non-** glob segment into a regexp body.
func globSegmentExpr(segment string) (string, error) {
	var out strings.Builder
	i := 0
	for i < len(segment) {
		switch segment[i] {
		case '*':
			out.WriteString("[^/]*")
			i++
		case '?':
			out.WriteString("[^/]")
			i++
		case '[':
			class, next, err := globClassExpr(segment, i)
			if err != nil {
				return "", err
			}
			out.WriteString(class)
			i = next
		default:
			j := i
			for j < len(segment) {
				c := segment[j]
				if c == '*' || c == '?' || c == '[' {
					break
				}
				j++
			}
			out.WriteString(regexp.QuoteMeta(segment[i:j]))
			i = j
		}
	}
	return out.String(), nil
}

// globClassExpr translates one character class starting at segment[open]
// ('['). It returns the regexp body and the index just past the closing
// ']'. A ']' in first position is literal, per glob rules.
func globClassExpr(segment string, open int) (string, int, error) {
	i := open + 1
	negated := false
	if i < len(segment) && (segment[i] == '!' || segment[i] == '^') {
		negated = true
		i++
	}
	var body strings.Builder
	closed := false
	for i < len(segment) {
		c := segment[i]
		if c == ']' && i > open+1 {
			closed = true
			i++
			break
		}
		if !isGlobClassByte(c) {
			return "", 0, fmt.Errorf("Glob 无效：字符类包含不支持的字节 0x%02x", c)
		}
		body.WriteByte(c)
		i++
	}
	if !closed {
		return "", 0, fmt.Errorf("Glob 无效：字符类未闭合")
	}
	if negated {
		return "[^" + body.String() + "]", i, nil
	}
	return "[" + body.String() + "]", i, nil
}

// isGlobClassByte accepts the conservative class alphabet (ASCII letters,
// digits, underscore, dash) so ranges work and regexp control bytes never
// enter a class.
func isGlobClassByte(c byte) bool {
	switch {
	case 'a' <= c && c <= 'z', 'A' <= c && c <= 'Z', '0' <= c && c <= '9':
		return true
	case c == '_' || c == '-':
		return true
	}
	return false
}
