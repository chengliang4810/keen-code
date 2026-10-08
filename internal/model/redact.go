package model

import (
	"net/url"
	"strings"
	"unicode/utf8"
)

// RedactedSecret is the fixed placeholder that replaces every secret value
// in error text (core/model/src/redaction.rs:7).
const RedactedSecret = "[REDACTED]"

const (
	// maxFieldNameBytes caps the scanned length of a field name candidate.
	maxFieldNameBytes = 80
	// maxURLCandidateBytes caps a single URL candidate; oversized candidates
	// are conservatively redacted whole.
	maxURLCandidateBytes = 64 * 1024
	// maxNestedURLDepth caps recursion into URL query/fragment values.
	maxNestedURLDepth = 8
)

// RedactErrorSecrets removes common authentication headers, sensitive fields,
// and URL credentials from error text. Only contexts with explicit secret
// semantics are rewritten; ordinary free text is never guessed at. Callers
// remain responsible for control-character cleanup and final length limits.
//
// This is a compact standard-library port of core/model/src/redaction.rs. It
// intentionally omits the multi-layer percent-encoding recursion beyond one
// net/url decode pass, JSON-escaped (\/) URL slash forms, and the bounded
// input window: see the package notes for the residual gaps.
func RedactErrorSecrets(input string) string {
	return redactErrorSecretsAtDepth(input, 0)
}

// RedactErrorSecretsBounded removes secrets and limits the output to
// maximumBytes without cutting inside a UTF-8 rune. Unlike the Rust bounded
// variant it redacts the whole input first and truncates afterwards, so the
// result never contains an unredacted suffix, at the cost of always scanning
// the full input.
func RedactErrorSecretsBounded(input string, maximumBytes int) string {
	if maximumBytes <= 0 || input == "" {
		return ""
	}
	return truncateUTF8(RedactErrorSecrets(input), maximumBytes)
}

// redactErrorSecretsAtDepth scans the input once, consuming whole
// candidates (URLs, bearer credentials, sensitive assignments) whenever one
// matches at the cursor.
func redactErrorSecretsAtDepth(input string, depth int) string {
	var out strings.Builder
	out.Grow(len(input))
	cursor := 0
	for cursor < len(input) {
		if replacement, end, matched := redactURLAt(input, cursor, depth); matched {
			out.WriteString(replacement)
			cursor = end
			continue
		}
		if replacement, end, matched := redactSensitiveAssignmentAt(input, cursor); matched {
			out.WriteString(replacement)
			cursor = end
			continue
		}
		if replacement, end, matched := redactBearerAt(input, cursor); matched {
			out.WriteString(replacement)
			cursor = end
			continue
		}
		_, size := utf8.DecodeRuneInString(input[cursor:])
		if size == 0 {
			size = 1
		}
		out.WriteString(input[cursor : cursor+size])
		cursor += size
	}
	return out.String()
}

// redactURLAt matches one http(s) URL candidate at start and strips userinfo,
// sensitive query/fragment fields, and secrets embedded in the path. It
// returns matched=false when no URL starts here.
func redactURLAt(input string, start, depth int) (string, int, bool) {
	if start > 0 && isASCIIAlphanumeric(input[start-1]) {
		return "", 0, false
	}
	schemeLength := urlSchemePrefixLength(input[start:])
	if schemeLength == 0 {
		return "", 0, false
	}
	if depth >= maxNestedURLDepth {
		// Conservative: refuse to dissect deeply nested URL values.
		end := urlCandidateEnd(input, start)
		if end == start {
			return "", 0, false
		}
		return RedactedSecret, end, true
	}

	end := urlCandidateEnd(input, start)
	if end == start {
		return "", 0, false
	}
	parsedEnd, trailing := trimURLTrailingPunctuation(input, start, end)
	if parsedEnd-start > maxURLCandidateBytes {
		return RedactedSecret + trailing, end, true
	}
	candidate := input[start:parsedEnd]
	parsed, err := url.Parse(candidate)
	if err != nil || !isWebScheme(parsed.Scheme) {
		if err != nil {
			// A malformed candidate with an explicit URL scheme cannot be
			// safely split into path and credentials; drop it whole.
			return RedactedSecret + trailing, end, true
		}
		return "", 0, false
	}

	changed := false
	if parsed.User != nil {
		parsed.User = nil
		changed = true
	}
	if redacted := redactErrorSecretsAtDepth(parsed.Path, depth+1); redacted != parsed.Path {
		parsed.Path = redacted
		changed = true
	}
	if redacted, found := redactQueryForm(parsed.RawQuery, depth+1, true); found {
		parsed.RawQuery = redacted
		changed = true
	}
	if redacted, found := redactQueryForm(parsed.Fragment, depth+1, false); found {
		parsed.Fragment = redacted
		changed = true
	}
	if !changed {
		return candidate + trailing, end, true
	}
	replacement := strings.ReplaceAll(parsed.String(), "%5BREDACTED%5D", RedactedSecret)
	return replacement + trailing, end, true
}

// redactQueryForm redacts sensitive name=value fields inside a raw query or
// a decoded fragment, preserving pair order and non-sensitive values. Nested
// URL values get one redaction pass on their decoded form.
func redactQueryForm(value string, depth int, decodePairs bool) (string, bool) {
	if value == "" {
		return "", false
	}
	pairs := strings.Split(value, "&")
	changed := false
	for i, pair := range pairs {
		name, separator, fieldValue := pair, "", ""
		if index := strings.IndexByte(pair, '='); index >= 0 {
			name, separator, fieldValue = pair[:index], "=", pair[index+1:]
		}
		nameForCheck := name
		if decodePairs {
			if decoded, err := url.QueryUnescape(name); err == nil {
				nameForCheck = decoded
			}
		}
		switch {
		case isSensitiveQueryName(nameForCheck):
			pairs[i] = name + separator + RedactedSecret
			changed = true
		case containsURLPrefix(fieldValue) || (decodePairs && containsEncodedURLPrefix(fieldValue)):
			decoded := fieldValue
			for range maxNestedURLDepth {
				next, err := url.QueryUnescape(decoded)
				if err != nil || next == decoded {
					break
				}
				decoded = next
			}
			if redacted := redactErrorSecretsAtDepth(decoded, depth+1); redacted != decoded {
				pairs[i] = name + separator + redacted
				changed = true
			}
		}
	}
	if !changed {
		return "", false
	}
	return strings.Join(pairs, "&"), true
}

// redactSensitiveAssignmentAt matches one sensitive field assignment in JSON,
// header, or plain key=value form at start.
func redactSensitiveAssignmentAt(input string, start int) (string, int, bool) {
	if start > 0 && isQuoteByte(input[start]) && input[start-1] == '\\' {
		return "", 0, false
	}
	keyStart, cursor, keyEnd, ok := scanFieldNameAt(input, start)
	if !ok {
		return "", 0, false
	}
	for keyEnd > keyStart && isHorizontalSpace(input[keyEnd-1]) {
		keyEnd--
	}
	if keyEnd == keyStart || !isSensitiveFieldName(input[keyStart:keyEnd]) {
		return "", 0, false
	}
	for cursor < len(input) && isHorizontalSpace(input[cursor]) {
		cursor++
	}
	if cursor >= len(input) {
		return "", 0, false
	}
	switch input[cursor] {
	case ':':
		cursor++
	case '=':
		cursor++
		if cursor < len(input) && input[cursor] == '>' {
			cursor++
		}
	default:
		return "", 0, false
	}
	for cursor < len(input) && isHorizontalSpace(input[cursor]) {
		cursor++
	}
	if cursor == len(input) {
		return "", 0, false
	}

	normalized, ok := normalizeFieldName(input[keyStart:keyEnd])
	if !ok {
		return "", 0, false
	}
	authorization := normalized == "authorization" || normalized == "proxyauthorization"
	structuredHeader := (authorization || normalized == "cookie" || normalized == "setcookie") &&
		!isQuoteByte(input[cursor]) && input[cursor] != '{' && input[cursor] != '[' && input[cursor] != '('
	if structuredHeader {
		end := structuredHeaderValueEnd(input, cursor)
		value := input[cursor:end]
		replacement := RedactedSecret
		if authorization {
			replacement = redactAuthScheme(value, true)
		}
		return input[start:cursor] + replacement, end, true
	}
	end, replacementValue, matched := redactValue(input, cursor, authorization)
	if !matched {
		return "", 0, false
	}
	return input[start:cursor] + replacementValue, end, true
}

// scanFieldNameAt recognizes either a quoted field name or an unquoted field
// name at start and returns the key bounds plus the cursor after the name.
func scanFieldNameAt(input string, start int) (keyStart, cursor, keyEnd int, ok bool) {
	if contentStart, delimiter, found := openingQuoteAt(input, start); found {
		maximumKeyEnd := min(contentStart+maxFieldNameBytes, len(input))
		cur := contentStart
		for {
			if end, closed := closingQuoteEndAt(input, cur, delimiter); closed {
				return contentStart, end, cur, true
			}
			if cur >= maximumKeyEnd || !isQuotedFieldNameByte(input[cur]) {
				return 0, 0, 0, false
			}
			cur++
		}
	}
	if !isUnquotedFieldNameByte(input[start]) || (start > 0 && isUnquotedFieldNameByte(input[start-1])) {
		return 0, 0, 0, false
	}
	keyStart = start
	maximumKeyEnd := min(keyStart+maxFieldNameBytes, len(input))
	cur := keyStart
	for cur < maximumKeyEnd && isQuotedFieldNameByte(input[cur]) {
		cur++
	}
	return keyStart, cur, cur, true
}

// redactBearerAt matches a standalone `Bearer <credential>` marker at start.
func redactBearerAt(input string, start int) (string, int, bool) {
	if start > 0 && isIdentifierByte(input[start-1]) {
		return "", 0, false
	}
	if !hasASCIIPrefix(input[start:], "bearer") {
		return "", 0, false
	}
	markerEnd := start + len("bearer")
	if markerEnd >= len(input) || !isHorizontalSpace(input[markerEnd]) {
		return "", 0, false
	}
	valueStart := markerEnd
	for valueStart < len(input) && isHorizontalSpace(input[valueStart]) {
		valueStart++
	}
	if valueStart == len(input) {
		return "", 0, false
	}
	end, replacementValue, matched := redactValue(input, valueStart, false)
	if !matched {
		return "", 0, false
	}
	return input[start:valueStart] + replacementValue, end, true
}

// redactValue replaces one field value starting at start and returns the end
// of the consumed value plus the safe replacement. preserveAuthScheme keeps
// the leading scheme token of authorization values.
func redactValue(input string, start int, preserveAuthScheme bool) (int, string, bool) {
	if length := redactionPlaceholderLength(input[start:]); length > 0 {
		end := start + length
		if !redactionPlaceholderHasSafeTerminator(input[end:]) {
			end = max(structuredHeaderValueEnd(input, end), end)
		}
		return end, RedactedSecret, true
	}
	if contentStart, delimiter, found := openingQuoteAt(input, start); found {
		contentEnd, end := quotedValueEnd(input, contentStart, delimiter)
		// Rebuild with the opening quote(s) preserved and the payload
		// redacted; the closing quote (if any) is preserved verbatim.
		redacted := redactAuthScheme(input[contentStart:contentEnd], preserveAuthScheme)
		replacement := input[start:contentStart] + redacted + input[contentEnd:end]
		return end, replacement, true
	}
	switch input[start] {
	case '{', '[', '(':
		end := balancedValueEnd(input, start)
		if end < 0 {
			end = lineEnd(input, start)
		}
		return end, RedactedSecret, true
	}
	if preserveAuthScheme {
		for _, scheme := range []string{"bearer", "basic"} {
			if !hasASCIIPrefix(input[start:], scheme) {
				continue
			}
			schemeEnd := start + len(scheme)
			if schemeEnd >= len(input) || !isHorizontalSpace(input[schemeEnd]) {
				continue
			}
			secretStart := schemeEnd
			for secretStart < len(input) && isHorizontalSpace(input[secretStart]) {
				secretStart++
			}
			end := unquotedValueEnd(input, secretStart)
			if end > secretStart {
				return end, input[start:secretStart] + RedactedSecret, true
			}
		}
	}
	end := unquotedValueEnd(input, start)
	if end == start {
		return 0, "", false
	}
	return end, RedactedSecret, true
}

// structuredHeaderValueEnd finds the end of an unquoted authorization or
// cookie header value, consuming structured parameters while keeping
// independent diagnostic fields that follow (core/model/src/redaction.rs:649-709).
func structuredHeaderValueEnd(input string, start int) int {
	inQuote := byte(0)
	cursor := start
	for cursor < len(input) {
		char := input[cursor]
		if inQuote != 0 {
			switch {
			case char == '\\':
				cursor = min(cursor+2, len(input))
			case char == inQuote:
				inQuote = 0
				cursor++
			case char == '\n' || char == '\r':
				return cursor
			default:
				cursor++
			}
			continue
		}
		switch {
		case char == '"' || char == '\'':
			inQuote = char
			cursor++
		case char == '\n' || char == '\r':
			return cursor
		case char == '\\' && cursor+1 < len(input) && (input[cursor+1] == 'n' || input[cursor+1] == 'r'):
			return cursor
		case isHorizontalSpace(char):
			diagnostic := cursor
			for diagnostic < len(input) && isHorizontalSpace(input[diagnostic]) {
				diagnostic++
			}
			if isIndependentDiagnosticAt(input, diagnostic) {
				return cursor
			}
			cursor = diagnostic
		case char == ',' || char == ';':
			cursor++
			for cursor < len(input) && isHorizontalSpace(input[cursor]) {
				cursor++
			}
		case char == '|':
			diagnostic := cursor + 1
			for diagnostic < len(input) && isHorizontalSpace(input[diagnostic]) {
				diagnostic++
			}
			if isIndependentDiagnosticAt(input, diagnostic) {
				return cursor
			}
			cursor++
		default:
			cursor++
		}
	}
	return len(input)
}

// isIndependentDiagnosticAt reports whether the whitespace-delimited token at
// start starts an independently reportable field whose value must survive
// redaction (request ids, status codes, retry hints, and similar context).
func isIndependentDiagnosticAt(input string, start int) bool {
	maximumKeyEnd := min(start+maxFieldNameBytes, len(input))
	cursor := start
	for cursor < maximumKeyEnd && isUnquotedFieldNameByte(input[cursor]) {
		cursor++
	}
	if cursor == start {
		return false
	}
	name, ok := normalizeFieldName(input[start:cursor])
	if !ok {
		return false
	}
	for cursor < len(input) && isHorizontalSpace(input[cursor]) {
		cursor++
	}
	if cursor >= len(input) || (input[cursor] != ':' && input[cursor] != '=') {
		return false
	}
	switch name {
	case "requestid", "traceid", "correlationid", "status", "statuscode", "httpstatus",
		"error", "errorcode", "code", "retryafter", "retryafterms", "detail", "details",
		"reason", "type", "authorization", "proxyauthorization", "cookie", "setcookie":
		return true
	default:
		return false
	}
}

// redactAuthScheme preserves the RFC 9110 scheme token of an authorization
// value but removes the credential after it.
func redactAuthScheme(value string, preserve bool) string {
	if preserve {
		schemeEnd := 0
		for schemeEnd < len(value) && isAuthSchemeByte(value[schemeEnd]) {
			schemeEnd++
		}
		if schemeEnd > 0 && schemeEnd < len(value) && isHorizontalSpace(value[schemeEnd]) {
			secretStart := schemeEnd
			for secretStart < len(value) && isHorizontalSpace(value[secretStart]) {
				secretStart++
			}
			return value[:schemeEnd] + value[schemeEnd:secretStart] + RedactedSecret
		}
	}
	return RedactedSecret
}

// quoteDelimiter describes an opening quote and its JSON escaping depth.
type quoteDelimiter struct {
	quote       byte
	backslashes int
}

// openingQuoteAt recognizes a plain or backslash-escaped opening quote at
// start and returns the payload start plus the delimiter.
func openingQuoteAt(input string, start int) (int, quoteDelimiter, bool) {
	if start >= len(input) {
		return 0, quoteDelimiter{}, false
	}
	if first := input[start]; first == '"' || first == '\'' {
		return start + 1, quoteDelimiter{quote: first}, true
	}
	if input[start] != '\\' {
		return 0, quoteDelimiter{}, false
	}
	cursor := start
	for cursor < len(input) && input[cursor] == '\\' {
		cursor++
	}
	if cursor >= len(input) || (input[cursor] != '"' && input[cursor] != '\'') {
		return 0, quoteDelimiter{}, false
	}
	return cursor + 1, quoteDelimiter{quote: input[cursor], backslashes: cursor - start}, true
}

// closingQuoteEndAt returns the position after the closing quote when input
// at start closes a value opened with the given delimiter.
func closingQuoteEndAt(input string, start int, delimiter quoteDelimiter) (int, bool) {
	if delimiter.backslashes == 0 {
		if start < len(input) && input[start] == delimiter.quote {
			return start + 1, true
		}
		return 0, false
	}
	if start > 0 && input[start-1] == '\\' {
		return 0, false
	}
	quoteAt := start + delimiter.backslashes
	if quoteAt >= len(input) {
		return 0, false
	}
	for i := start; i < quoteAt; i++ {
		if input[i] != '\\' {
			return 0, false
		}
	}
	if input[quoteAt] != delimiter.quote {
		return 0, false
	}
	return quoteAt + 1, true
}

// quotedValueEnd scans a quoted value and returns the payload end and the
// value end; an unterminated value consumes to the end of input and keeps
// the closing quote absent (core/model/src/redaction.rs:895-909).
func quotedValueEnd(input string, start int, delimiter quoteDelimiter) (int, int) {
	cursor := start
	for cursor < len(input) {
		if end, closed := closingQuoteEndAt(input, cursor, delimiter); closed {
			return cursor, end
		}
		if delimiter.backslashes == 0 && input[cursor] == '\\' {
			cursor = min(cursor+2, len(input))
			continue
		}
		if input[cursor] == '\n' || input[cursor] == '\r' {
			return cursor, cursor
		}
		cursor++
	}
	return len(input), len(input)
}

// balancedValueEnd finds the matching close of a JSON or debug container;
// it returns -1 when unbalanced across a line break.
func balancedValueEnd(input string, start int) int {
	if start >= len(input) {
		return -1
	}
	var closing byte
	switch input[start] {
	case '{':
		closing = '}'
	case '[':
		closing = ']'
	case '(':
		closing = ')'
	default:
		return -1
	}
	depth := 0
	inQuote := byte(0)
	cursor := start
	for cursor < len(input) {
		char := input[cursor]
		if inQuote != 0 {
			switch {
			case char == '\\':
				cursor = min(cursor+2, len(input))
			case char == inQuote:
				inQuote = 0
				cursor++
			default:
				cursor++
			}
			continue
		}
		switch {
		case char == '"' || char == '\'':
			inQuote = char
			cursor++
		case char == input[start]:
			depth++
			cursor++
		case char == closing:
			depth--
			cursor++
			if depth == 0 {
				return cursor
			}
		case char == '\n' || char == '\r':
			return -1
		default:
			cursor++
		}
	}
	return -1
}

// unquotedValueEnd consumes an unquoted value up to its natural terminator
// without swallowing following context (core/model/src/redaction.rs:1014-1032).
func unquotedValueEnd(input string, start int) int {
	cursor := start
	for cursor < len(input) {
		char := input[cursor]
		if isASCIISpace(char) || strings.ContainsRune(",;&}])\"'", rune(char)) {
			break
		}
		if char == '\\' && cursor+1 < len(input) &&
			strings.ContainsRune("nrt\"'", rune(input[cursor+1])) {
			break
		}
		cursor++
	}
	return cursor
}

// lineEnd returns the end of the current line.
func lineEnd(input string, start int) int {
	if index := strings.IndexAny(input[start:], "\n\r"); index >= 0 {
		return start + index
	}
	return len(input)
}

// redactionPlaceholderLength matches a fixed redaction placeholder prefix.
func redactionPlaceholderLength(value string) int {
	for _, placeholder := range []string{RedactedSecret, "<redacted>"} {
		if hasASCIIPrefix(value, placeholder) {
			return len(placeholder)
		}
	}
	return 0
}

// redactionPlaceholderHasSafeTerminator reports whether the placeholder is
// already a complete value (field end or a following independent field).
func redactionPlaceholderHasSafeTerminator(remainder string) bool {
	if remainder == "" {
		return true
	}
	if strings.HasPrefix(remainder, "\\\"") || strings.HasPrefix(remainder, "\\'") {
		remainder = remainder[2:]
	} else if isQuoteByte(remainder[0]) {
		remainder = remainder[1:]
	}
	beforeWhitespace := len(remainder)
	remainder = strings.TrimLeft(remainder, " \t")
	if remainder == "" || strings.ContainsRune(",;&}])\r\n", rune(remainder[0])) {
		return true
	}
	return len(remainder) < beforeWhitespace && looksLikeAssignment(remainder)
}

// looksLikeAssignment reports whether the text starts a name/separator pair.
func looksLikeAssignment(value string) bool {
	cursor := 0
	for cursor < len(value) && isUnquotedFieldNameByte(value[cursor]) {
		cursor++
	}
	if cursor == 0 {
		return false
	}
	for cursor < len(value) && isHorizontalSpace(value[cursor]) {
		cursor++
	}
	return cursor < len(value) && (value[cursor] == ':' || value[cursor] == '=')
}

// isSensitiveFieldName reports whether a header, JSON, or query field name
// explicitly carries a secret (core/model/src/redaction.rs:1042-1084).
func isSensitiveFieldName(name string) bool {
	allowSuffix := !strings.ContainsAny(name, " \t")
	normalized, ok := normalizeFieldName(name)
	if !ok {
		return false
	}
	switch normalized {
	case "auth", "authorization", "proxyauthorization", "apikey", "xapikey", "token",
		"accesstoken", "refreshtoken", "authtoken", "bearertoken", "idtoken",
		"sessiontoken", "password", "passwd", "pwd", "secret", "clientsecret",
		"apisecret", "credential", "credentials", "cookie", "setcookie",
		"signature", "sig":
		return true
	}
	if !allowSuffix {
		return false
	}
	for _, suffix := range []string{"apikey", "password", "passwd", "secret", "privatekey",
		"accesskey", "secretkey", "subscriptionkey", "signature"} {
		if strings.HasSuffix(normalized, suffix) {
			return true
		}
	}
	return strings.HasSuffix(normalized, "token") && !isTokenMetricName(normalized)
}

// isSensitiveQueryName adds the common session-signature query field names.
func isSensitiveQueryName(name string) bool {
	if isSensitiveFieldName(name) {
		return true
	}
	normalized, ok := normalizeFieldName(name)
	if !ok {
		return false
	}
	switch normalized {
	case "auth", "key", "session", "sessionid", "csrf", "nonce", "jwt":
		return true
	default:
		return false
	}
}

// isTokenMetricName keeps token accounting fields visible; they are ordinary
// error context needed to diagnose budget problems
// (core/model/src/redaction.rs:1100-1127).
func isTokenMetricName(name string) bool {
	switch name {
	case "tokens", "maxtoken", "maxtokens", "maxinputtokens", "maxoutputtokens",
		"inputtoken", "inputtokens", "outputtoken", "outputtokens", "totaltoken",
		"totaltokens", "completiontoken", "completiontokens", "prompttoken",
		"prompttokens", "cachedtoken", "cachedtokens", "reasoningtoken",
		"reasoningtokens", "tokencount", "tokenlimit", "tokenbudget", "tokenusage":
		return true
	default:
		return false
	}
}

// normalizeFieldName collapses snake/kebab/dotted/camel spellings of a field
// name into the comparable ASCII form; non-ASCII names never match.
func normalizeFieldName(name string) (string, bool) {
	if name == "" || !isASCII(name) {
		return "", false
	}
	var builder strings.Builder
	for i := 0; i < len(name); i++ {
		char := name[i]
		if isASCIIAlphanumeric(char) {
			builder.WriteByte(lowerASCIIByte(char))
		}
	}
	normalized := builder.String()
	return normalized, normalized != ""
}

// --- small character/lexical helpers ---

func isASCII(value string) bool {
	for i := 0; i < len(value); i++ {
		if value[i] >= utf8.RuneSelf {
			return false
		}
	}
	return true
}

func isASCIIAlphanumeric(char byte) bool {
	return 'a' <= char && char <= 'z' || 'A' <= char && char <= 'Z' || '0' <= char && char <= '9'
}

func lowerASCIIByte(char byte) byte {
	if 'A' <= char && char <= 'Z' {
		return char + ('a' - 'A')
	}
	return char
}

func isHorizontalSpace(char byte) bool { return char == ' ' || char == '\t' }

func isASCIISpace(char byte) bool {
	return char == ' ' || char == '\t' || char == '\n' || char == '\r' || char == '\v' || char == '\f'
}

func isQuoteByte(char byte) bool { return char == '"' || char == '\'' }

func isIdentifierByte(char byte) bool {
	return isASCIIAlphanumeric(char) || char == '_' || char == '-'
}

func isUnquotedFieldNameByte(char byte) bool {
	return isASCIIAlphanumeric(char) || char == '_' || char == '-' || char == '.'
}

func isQuotedFieldNameByte(char byte) bool {
	return isUnquotedFieldNameByte(char) || isHorizontalSpace(char)
}

func isAuthSchemeByte(char byte) bool {
	return isASCIIAlphanumeric(char) || strings.ContainsRune("!#$%&'*+-.^_`|~", rune(char))
}

// hasASCIIPrefix is a case-insensitive ASCII prefix test.
func hasASCIIPrefix(value, prefix string) bool {
	if len(value) < len(prefix) {
		return false
	}
	return strings.EqualFold(value[:len(prefix)], prefix)
}

// urlSchemePrefixLength returns the length of an http/https scheme prefix.
func urlSchemePrefixLength(value string) int {
	for _, scheme := range []string{"http://", "https://"} {
		if hasASCIIPrefix(value, scheme) {
			return len(scheme)
		}
	}
	return 0
}

func isWebScheme(scheme string) bool {
	return strings.EqualFold(scheme, "http") || strings.EqualFold(scheme, "https")
}

// urlCandidateEnd consumes one URL candidate up to a whitespace, control,
// quote, angle bracket, or backslash terminator.
func urlCandidateEnd(input string, start int) int {
	cursor := start
	for cursor < len(input) {
		char := input[cursor]
		if isASCIISpace(char) || char == 0x7f || char < 0x20 ||
			isQuoteByte(char) || char == '<' || char == '>' || char == '\\' {
			break
		}
		cursor++
	}
	return cursor
}

// trimURLTrailingPunctuation strips closing ASCII punctuation that belongs
// to the surrounding sentence, not the URL.
func trimURLTrailingPunctuation(input string, start, end int) (int, string) {
	parsedEnd := end
	for parsedEnd > start && strings.IndexByte(",;:!?.)]}", input[parsedEnd-1]) >= 0 {
		parsedEnd--
	}
	return parsedEnd, input[parsedEnd:end]
}

// containsURLPrefix reports a plain http(s) URL start.
func containsURLPrefix(value string) bool {
	return hasASCIIPrefix(value, "http://") || hasASCIIPrefix(value, "https://")
}

// containsEncodedURLPrefix reports a percent-encoded http(s) URL start.
func containsEncodedURLPrefix(value string) bool {
	return strings.Contains(value, "http%3A%2F%2F") || strings.Contains(value, "https%3A%2F%2F")
}

// truncateUTF8 limits value to maximumBytes without cutting inside a rune.
func truncateUTF8(value string, maximumBytes int) string {
	if len(value) <= maximumBytes {
		return value
	}
	end := maximumBytes
	for end > 0 && !utf8.RuneStart(value[end]) {
		end--
	}
	return value[:end]
}
