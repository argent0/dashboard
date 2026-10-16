
// Minimal JSON for the wire form: String, Long, Double, Boolean, List and
// Map<String, Any?> in, the same out. File-private, so no dependency.
private object Wire {
    fun write(v: Any?): String = StringBuilder().also { put(v, it) }.toString()

    private fun put(v: Any?, sb: StringBuilder) {
        when (v) {
            is String -> {
                sb.append('"')
                for (c in v) when {
                    c == '"' -> sb.append("\\\"")
                    c == '\\' -> sb.append("\\\\")
                    c < ' ' -> sb.append("\\u%04x".format(c.code))
                    else -> sb.append(c)
                }
                sb.append('"')
            }
            is Long, is Boolean, is Double -> sb.append(v)
            is List<*> -> {
                sb.append('[')
                v.forEachIndexed { i, x -> if (i > 0) sb.append(','); put(x, sb) }
                sb.append(']')
            }
            is Map<*, *> -> {
                sb.append('{')
                var first = true
                for ((k, x) in v) {
                    if (!first) sb.append(',')
                    first = false
                    put(k as String, sb); sb.append(':'); put(x, sb)
                }
                sb.append('}')
            }
            else -> throw IllegalArgumentException("cannot encode ${v?.javaClass}")
        }
    }

    fun read(s: String): Any? {
        val p = Parser(s)
        val v = p.value()
        p.ws()
        if (p.i != s.length) p.fail("trailing input")
        return v
    }

    private class Parser(val s: String) {
        var i = 0
        fun fail(m: String): Nothing = throw IllegalArgumentException("$m at $i")
        fun ws() { while (i < s.length && s[i].isWhitespace()) i++ }
        fun expect(c: Char) { if (i >= s.length || s[i] != c) fail("expected '$c'"); i++ }
        fun value(): Any? {
            ws()
            if (i >= s.length) fail("unexpected end")
            return when (val c = s[i]) {
                '{' -> obj()
                '[' -> arr()
                '"' -> str()
                't' -> lit("true", true)
                'f' -> lit("false", false)
                else -> if (c == '-' || c.isDigit()) num() else fail("unexpected '$c'")
            }
        }
        fun lit(w: String, v: Boolean): Boolean {
            if (!s.startsWith(w, i)) fail("bad literal")
            i += w.length
            return v
        }
        fun num(): Any {
            val st = i
            while (i < s.length && (s[i].isDigit() || s[i] in "+-.eE")) i++
            val t = s.substring(st, i)
            return if (t.any { it in ".eE" }) t.toDouble() else t.toLong()
        }
        fun str(): String {
            expect('"')
            val sb = StringBuilder()
            while (true) {
                if (i >= s.length) fail("unterminated string")
                val c = s[i++]
                when (c) {
                    '"' -> return sb.toString()
                    '\\' -> {
                        if (i >= s.length) fail("unterminated escape")
                        when (val e = s[i++]) {
                            'n' -> sb.append('\n'); 't' -> sb.append('\t'); 'r' -> sb.append('\r')
                            'b' -> sb.append('\b'); 'f' -> sb.append('\u000c')
                            'u' -> { sb.append(s.substring(i, i + 4).toInt(16).toChar()); i += 4 }
                            '"', '\\', '/' -> sb.append(e)
                            else -> fail("bad escape")
                        }
                    }
                    else -> sb.append(c)
                }
            }
        }
        fun arr(): List<Any?> {
            expect('[')
            val out = ArrayList<Any?>()
            ws()
            if (s.getOrNull(i) == ']') { i++; return out }
            while (true) {
                out.add(value()); ws()
                if (s.getOrNull(i) == ',') { i++; continue }
                expect(']'); return out
            }
        }
        fun obj(): Map<String, Any?> {
            expect('{')
            val out = LinkedHashMap<String, Any?>()
            ws()
            if (s.getOrNull(i) == '}') { i++; return out }
            while (true) {
                ws(); val k = str(); ws(); expect(':')
                out[k] = value(); ws()
                if (s.getOrNull(i) == ',') { i++; continue }
                expect('}'); return out
            }
        }
    }
}
