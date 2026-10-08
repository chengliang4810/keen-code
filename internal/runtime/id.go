package runtime

import (
	"crypto/rand"
	"errors"
	"fmt"
	"time"
)

// Crockford Base32 alphabet (ULID encoding, no I/L/O/U). Session and event
// identifiers are time ordered 128-bit values encoded in this alphabet:
// crypto/rand only, no third-party dependency
// (docs/go-migration.md §5.5 SessionMeta.ID).
const crockfordAlphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

// ulidEncodedLen is the length of a 128-bit value in Crockford Base32.
const ulidEncodedLen = 26

// errBadID marks identifiers that are not 26 character Crockford Base32
// strings; session directory names are validated against this shape before
// any filesystem access.
var errBadID = errors.New("标识必须是 26 位 Crockford Base32 字符串")

// newID returns a ULID-compatible identifier: 48 bits of unix millisecond
// timestamp followed by 80 bits of crypto-random entropy.
func newID() string {
	var bytes [16]byte
	ms := uint64(time.Now().UnixMilli())
	bytes[0] = byte(ms >> 40)
	bytes[1] = byte(ms >> 32)
	bytes[2] = byte(ms >> 24)
	bytes[3] = byte(ms >> 16)
	bytes[4] = byte(ms >> 8)
	bytes[5] = byte(ms)
	if _, err := rand.Read(bytes[6:]); err != nil {
		// crypto/rand failing means the platform entropy source is broken;
		// there is no safe identifier to hand out.
		panic(fmt.Sprintf("runtime: 随机数源不可用: %v", err))
	}
	return encodeCrockford(bytes)
}

// encodeCrockford encodes 16 bytes as 26 Crockford Base32 characters,
// most significant bit first (ULID string layout). The final character
// carries only three value bits; the two most significant bits of the
// 128-bit space stay zero (timestamp high bits).
func encodeCrockford(bytes [16]byte) string {
	out := make([]byte, ulidEncodedLen)
	for i := 0; i < ulidEncodedLen; i++ {
		var val byte
		for k := 0; k < 5; k++ {
			pos := 5*i + k
			val <<= 1
			if pos < 128 && bytes[pos/8]&(0x80>>uint(pos%8)) != 0 {
				val |= 1
			}
		}
		out[i] = crockfordAlphabet[val]
	}
	return string(out)
}

// validID reports whether id is a syntactically valid generated identifier.
// Generated IDs are uppercase Crockford Base32; decoding accepts lowercase
// for robustness on read paths.
func validID(id string) bool {
	if len(id) != ulidEncodedLen {
		return false
	}
	for i := 0; i < len(id); i++ {
		c := id[i]
		switch {
		case 'A' <= c && c <= 'Z':
			switch c {
			case 'I', 'L', 'O', 'U':
				return false
			}
		case 'a' <= c && c <= 'z':
			switch c {
			case 'i', 'l', 'o', 'u':
				return false
			}
		case '0' <= c && c <= '9':
		default:
			return false
		}
	}
	return true
}
