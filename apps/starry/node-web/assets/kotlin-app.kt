// kotlin-app.kt — source for the StarryOS node-web Kotlin/JS carpet.
//
// Compiled by the Kotlin 2.0.21 Kotlin/JS IR backend to a self-contained commonjs
// module (kotlin-app.js) that self-executes main() on load and prints six golden
// lines to stdout (compared byte-for-byte against kotlin-REF.out by KotlinJsCarpet.js).
//
// Deterministic: no Date / Math.random / network / timestamps. Exercises a broad slice
// of Kotlin language + stdlib features (one golden line each):
//   line 1  data class + componentN/copy       -> points=(21,12);(23,14);(25,16)
//   line 2  sealed class + exhaustive `when`    -> areas=12,12,3 total=27
//   line 3  higher-order fns / lambdas / map / filter / sum -> evens^2 sum=220
//   line 4  generics + extension functions      -> second-of=b
//   line 5  recursion                           -> fib(15)=610
//   line 6  null-safety (?. ?: !!)              -> nullsafe=YES

// --- line 1: data class, copy(), componentN via destructuring ---------------
data class Point(val x: Int, val y: Int) {
    override fun toString(): String = "($x,$y)"
}

// --- line 2: sealed class hierarchy + exhaustive `when` ---------------------
sealed class Shape
data class Rect(val w: Int, val h: Int) : Shape()
data class Tri(val base: Int, val height: Int) : Shape()
data class Circle(val r: Int) : Shape()

fun area(s: Shape): Int = when (s) {          // exhaustive over the sealed hierarchy
    is Rect -> s.w * s.h
    is Tri -> s.base * s.height / 2
    is Circle -> (3.14159 * s.r * s.r).toInt()
}

// --- line 4: generic extension function ------------------------------------
fun <T> List<T>.secondOf(): T = this[1]

// --- line 5: recursion ------------------------------------------------------
fun fib(n: Int): Int = if (n < 2) n else fib(n - 1) + fib(n - 2)

fun main() {
    // line 1: base points, each mapped through copy() with a scale+translate transform.
    val points = listOf(Point(10, 5), Point(11, 6), Point(12, 7))
        .map { p -> p.copy(x = p.x * 2 + 1, y = p.y * 2 + 2) }
    println("points=" + points.joinToString(";"))

    // line 2: areas of a mixed sealed-class list + their total.
    val shapes: List<Shape> = listOf(Rect(4, 3), Tri(8, 3), Circle(1))
    val areas = shapes.map { area(it) }
    println("areas=" + areas.joinToString(",") + " total=" + areas.sum())

    // line 3: filter evens in 1..10, square each, sum.
    val evensSq = (1..10).filter { it % 2 == 0 }.map { it * it }.sum()
    println("evens^2 sum=" + evensSq)

    // line 4: generic extension fn on a List<String>.
    println("second-of=" + listOf("a", "b", "c").secondOf())

    // line 5: recursion.
    println("fib(15)=" + fib(15))

    // line 6: null-safety operators ?. ?: !!.
    val opt: String? = "yes"
    val forcedLen = opt!!.length                  // !!
    val verdict = opt?.let { "YES" } ?: "NO"      // ?. and ?:
    println("nullsafe=" + if (forcedLen == 3) verdict else "NO")
}
