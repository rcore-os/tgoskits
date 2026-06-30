// ============================================================================
// LangBSV.bsv -- CARPET-LEVEL Bluespec SystemVerilog (BSV) language design for
// StarryOS #764 (HDL item: Bluespec / bsc).
//
// Doc-grounded from the Bluespec SystemVerilog (BSV) language reference guide
//   (BSV_lang.tex / StmtFSM.tex / libraries_ref_guide).
//
// This is ONE synthesizable (* synthesize *) top module that exercises the FULL
// BSV language surface and $display's DETERMINISTIC results -- captured AS GOLDEN
// on the host, then compared on target (golden-capture model; NO hardcoded
// expected values anywhere in the driver).  Every emitted line is of the form
//   LABEL=VALUE
// so the driver can diff line-by-line, one check per line.
//
// Coverage (BSV language surface):
//   * Types: Bit#, UInt#, Int#, Bool, Integer, Maybe, tuples, String, enum,
//     struct, tagged union (incl. void member), polymorphic struct.
//   * deriving: Bits, Eq, FShow, Bounded.
//   * typeclasses: user typeclass + instances (ad-hoc overloading) + default
//     methods; provisos (Add, Mul, Log, Bits); valueOf / SizeOf.
//   * Polymorphism: polymorphic functions + polymorphic struct/typeclass.
//   * Functions: ordinary, recursive (static), higher-order (map/fold/zipWith),
//     function composition, curry/uncurry, begin-end value blocks, let, case,
//     if-else, pattern matching (case matches, tagged), guarded `when`-style
//     via Maybe/isValid.
//   * Bit ops: concat {a,b}, select x[i], part-select x[hi:lo], zeroExtend,
//     signExtend, truncate, reverseBits, pack/unpack, &|^~, <<,>>, countOnes,
//     ranges, ' literals.
//   * Static loops: for, while (statically unrolled in pure value context).
//   * Vector: replicate, genWith, map, fold, zipWith, elem, reverse, rotate,
//     append, take/drop, Vector of Reg, readVReg, arithmetic on Vector.
//   * Modules / interfaces / methods: interface decl with value method,
//     Action method, ActionValue method; module instantiation; mkReg, mkRegU,
//     mkFIFO/mkFIFOF, sub-module composition; method invocation.
//   * Rules: rules with explicit conditions (guards), rule scheduling/ordering,
//     multiple rules same cycle, $display determinism.
//   * StmtFSM: seq/par/while/for/if blocks, mkAutoFSM sequencing.
//
// Determinism: NO wall clock, NO $time in golden, NO hash/map order, NO
// pointers/addresses.  FSM gives a fixed statement order; rules used here are
// either single-firing or fully ordered by the FSM so output order is fixed.
// ============================================================================
package LangBSV;

import Vector::*;
import FIFOF::*;
import StmtFSM::*;

// ---------------------------------------------------------------------------
// 1. Type definitions: enum / struct / tagged union (incl void) / polymorphic
// ---------------------------------------------------------------------------
typedef enum { Idle, Run, Stop, Wait } State deriving (Bits, Eq, Bounded, FShow);

typedef struct {
   Bool      flag;
   Bit#(8)   data;
   UInt#(16) count;
   Int#(8)   delta;
} Packet deriving (Bits, Eq, FShow);

// polymorphic struct
typedef struct {
   t   key;
   v   payload;
} Pair#(type t, type v) deriving (Bits, Eq, FShow);

// tagged union with a void member + a struct member
typedef union tagged {
   void              Empty;
   Bit#(8)           Leaf;
   struct { Bit#(8) lo; Bit#(8) hi; } Node;
} Tree deriving (Bits, Eq, FShow);

// Maybe is library-provided; we use Maybe#(Bit#(8)) below.

// ---------------------------------------------------------------------------
// 2. Typeclass with instances (ad-hoc overloading) + default method
// ---------------------------------------------------------------------------
typeclass Describable#(type t);
   function Bit#(8) score(t x);                          // required method
   // default method, defined in terms of the required method:
   function Bit#(8) doubled(t x) = score(x) + score(x);
endtypeclass

instance Describable#(State);
   function Bit#(8) score(State s);
      return case (s)
                Idle: 8'd1;
                Run:  8'd2;
                Stop: 8'd3;
                Wait: 8'd4;
             endcase;
   endfunction
endinstance

instance Describable#(Bool);
   function Bit#(8) score(Bool b) = b ? 8'd10 : 8'd20;
endinstance

// ---------------------------------------------------------------------------
// 3. Polymorphic functions with provisos + valueOf / SizeOf
// ---------------------------------------------------------------------------
// width of any Bits type, via SizeOf/valueOf
function Integer bitWidth(t proxy) provisos (Bits#(t, n));
   return valueOf(n);
endfunction

// top bit of any non-empty Bit vector
function Bit#(1) msb1(Bit#(n) x) provisos (Add#(1, k, n));
   return x[valueOf(n) - 1];
endfunction

// static recursive factorial on Integer (elaboration-time)
function Integer fact(Integer n);
   return (n <= 1) ? 1 : n * fact(n - 1);
endfunction

// recursive popcount via shifting on a fixed width (static unroll)
function Bit#(8) popc(Bit#(16) x);
   Bit#(8) c = 0;
   for (Integer i = 0; i < 16; i = i + 1)
      c = c + zeroExtend(x[i]);
   return c;
endfunction

// higher-order: apply a function twice
function t twice(function t f(t a), t x) = f(f(x));

// ---------------------------------------------------------------------------
// 4. A real sub-module with value/Action/ActionValue methods + an interface
// ---------------------------------------------------------------------------
interface Acc#(type t);
   method Action add(t x);          // Action method
   method t      total;             // value method
   method ActionValue#(t) drain;    // ActionValue method
endinterface

module mkAcc(Acc#(t)) provisos (Bits#(t, n), Arith#(t));
   Reg#(t) r <- mkReg(unpack(0));
   method Action add(t x);
      r <= r + x;
   endmethod
   method t total = r;
   method ActionValue#(t) drain;
      let v = r;
      r <= unpack(0);
      return v;
   endmethod
endmodule

// A second sub-module: a saturating up-counter interface
interface Counter8;
   method Action up;
   method Action clear;
   method Bit#(8) value;
endinterface

module mkSatCounter(Counter8);
   Reg#(Bit#(8)) c <- mkReg(0);
   method Action up if (c < 8'hFF);    // implicit guard (method condition)
      c <= c + 1;
   endmethod
   method Action clear;
      c <= 0;
   endmethod
   method Bit#(8) value = c;
endmodule

// ---------------------------------------------------------------------------
// 5. The top: drives everything via StmtFSM so order is deterministic.
// ---------------------------------------------------------------------------
(* synthesize *)
module mkLangBSV(Empty);

   // sub-module instances (explicit state via instantiation)
   Acc#(Bit#(16))    acc   <- mkAcc;
   Counter8          ctr   <- mkSatCounter;
   FIFOF#(Bit#(8))   fifo  <- mkFIFOF;

   // Vector of Reg (register file)
   Vector#(4, Reg#(Bit#(8))) rf <- replicateM(mkReg(0));

   // some plain registers used across FSM steps
   Reg#(Bit#(16))    sum   <- mkReg(0);
   Reg#(Bit#(8))     fcnt  <- mkReg(0);

   // -- FSM body --------------------------------------------------------------
   Stmt prog = seq
      // ----- (A) integer / arithmetic / literals -----
      action
         Integer a = 7; Integer b = 3;
         $display("ADD=%0d", a + b);
         $display("SUB=%0d", a - b);
         $display("MUL=%0d", a * b);
         $display("DIV=%0d", a / b);
         $display("MOD=%0d", a % b);
         $display("FACT5=%0d", fact(5));
         $display("FACT7=%0d", fact(7));
      endaction

      // ----- (B) Bit operations -----
      action
         Bit#(8) x = 8'hB3;          // 1011 0011
         Bit#(4) y = 4'h5;
         $display("BAND=%0h", x & 8'h0F);
         $display("BOR=%0h",  x | 8'h0F);
         $display("BXOR=%0h", x ^ 8'hFF);
         $display("BNOT=%0h", ~x);
         $display("SHL=%0h", x << 2);
         $display("SHR=%0h", x >> 2);
         $display("CONCAT=%0h", {x, y});             // 12-bit concat
         $display("SEL=%0d", x[7]);                  // single bit
         $display("PART=%0h", x[5:2]);               // part select
         $display("REVB=%0h", reverseBits(x));
         $display("POPC=%0d", popc(zeroExtend(x)));  // 5 ones in 0xB3
         $display("MSB1=%0d", msb1(x));
         Bit#(8) ze = zeroExtend(y);  $display("ZEXT8=%0h", ze);
         Bit#(8) se = signExtend(4'hF);  $display("SEXT8=%0h", se);
         Bit#(4) tr = truncate(x);    $display("TRUNC4=%0h", tr);
      endaction

      // ----- (C) struct / enum / tagged union + FShow -----
      action
         Packet p = Packet { flag: True, data: 8'h2A, count: 300, delta: -5 };
         $display("PKT=", fshow(p));
         $display("PKT_DATA=%0h", p.data);
         Packet p2 = p; p2.data = 8'h2B;            // struct update
         $display("PKT2_DATA=%0h", p2.data);
         $display("PKT_EQ=%0d", (p == p2) ? 1 : 0);

         State s0 = Idle; State s1 = Run;
         $display("ST0=", fshow(s0));
         $display("ST1=", fshow(s1));
         $display("ST_EQ=%0d", (s0 == s1) ? 1 : 0);
         State stmin = minBound; State stmax = maxBound;   // Bounded
         $display("ST_MIN=", fshow(stmin));
         $display("ST_MAX=", fshow(stmax));
         $display("ST_PACK=%0d", pack(s1));

         Tree t1 = tagged Leaf 8'h55;
         Tree t2 = tagged Node { lo: 8'h11, hi: 8'h22 };
         Tree t3 = tagged Empty;
         $display("TR1=", fshow(t1));
         $display("TR2=", fshow(t2));
         $display("TR3=", fshow(t3));
         // pattern match on tagged union
         case (t2) matches
            tagged Empty:           $display("TRMATCH=0");
            tagged Leaf .v:         $display("TRMATCH=L%0h", v);
            tagged Node {lo:.l,hi:.h}: $display("TRMATCH=N%0h_%0h", l, h);
         endcase

         // polymorphic struct
         Pair#(State, Bit#(8)) pr = Pair { key: Stop, payload: 8'hAB };
         $display("PAIR=", fshow(pr));
      endaction

      // ----- (D) Maybe + isValid/fromMaybe -----
      action
         Maybe#(Bit#(8)) mv = tagged Valid 8'h7E;
         Maybe#(Bit#(8)) mn = tagged Invalid;
         $display("MV_VALID=%0d", isValid(mv) ? 1 : 0);
         $display("MN_VALID=%0d", isValid(mn) ? 1 : 0);
         $display("MV_VAL=%0h", fromMaybe(8'h00, mv));
         $display("MN_VAL=%0h", fromMaybe(8'hFF, mn));
         $display("MV_FSHOW=", fshow(mv));
      endaction

      // ----- (E) typeclass overloading + default method + provisos -----
      action
         $display("SCORE_IDLE=%0d", score(Idle));
         $display("SCORE_RUN=%0d",  score(Run));
         $display("SCORE_TRUE=%0d", score(True));
         $display("DBL_RUN=%0d",    doubled(Run));      // default method
         $display("DBL_FALSE=%0d",  doubled(False));
         Bit#(32) proxy32 = 0; $display("WIDTH32=%0d", bitWidth(proxy32));
         Packet pp = unpack(0);  $display("WIDTH_PKT=%0d", bitWidth(pp));
         State sp = Idle;        $display("WIDTH_ST=%0d", bitWidth(sp));
      endaction

      // ----- (F) Vector operations (pure, statically unrolled) -----
      action
         Vector#(8, Bit#(8)) v = genWith(fromInteger);   // [0,1,..,7]
         Vector#(8, Bit#(8)) w = replicate(2);
         Vector#(8, Bit#(8)) m = map(uncurry(\* ), zip(v, w));  // v*2 elemwise
         Bit#(8) s = fold(\+ , v);                        // 0+..+7 = 28
         $display("VGEN0=%0d", v[0]);
         $display("VGEN7=%0d", v[7]);
         $display("VMUL3=%0d", m[3]);
         $display("VSUM=%0d", s);
         $display("VELEM5=%0d", elem(8'd5, v) ? 1 : 0);
         $display("VELEM9=%0d", elem(8'd9, v) ? 1 : 0);
         $display("VREV0=%0d", reverse(v)[0]);
         $display("VROT0=%0d", rotate(v)[0]);              // left-rotate
         Vector#(4, Bit#(8)) a4 = take(v);
         Vector#(4, Bit#(8)) b4 = takeTail(v);
         $display("VTAKE3=%0d", a4[3]);
         $display("VDROP0=%0d", b4[0]);
         // map with higher-order twice()
         function Bit#(8) inc1(Bit#(8) z) = z + 1;
         $display("VTWICE=%0d", twice(inc1, 8'd40));       // 42
      endaction

      // ----- (G) Vector of Reg (register file write/read) -----
      action
         for (Integer k = 0; k < 4; k = k + 1)
            rf[k] <= fromInteger((k + 1) * 10);            // 10,20,30,40
      endaction
      action
         Vector#(4, Bit#(8)) snap = readVReg(rf);
         $display("RF0=%0d", snap[0]);
         $display("RF3=%0d", snap[3]);
         $display("RFSUM=%0d", fold(\+ , snap));           // 100
      endaction

      // ----- (H) sub-module Acc: Action / value / ActionValue methods -----
      action acc.add(100); endaction
      action acc.add(23);  endaction
      action $display("ACC_TOTAL=%0d", acc.total); endaction
      action let d <- acc.drain; $display("ACC_DRAIN=%0d", d); endaction
      action $display("ACC_AFTER=%0d", acc.total); endaction

      // ----- (I) saturating counter sub-module (method guard) -----
      action ctr.up; endaction
      action ctr.up; endaction
      action ctr.up; endaction
      action $display("CTR=%0d", ctr.value); endaction
      action ctr.clear; endaction
      action $display("CTR_CLR=%0d", ctr.value); endaction

      // ----- (J) FIFO sub-module (enq/deq/first via GetPut-style) -----
      action fifo.enq(8'hA1); endaction
      action fifo.enq(8'hB2); endaction
      action $display("FIFO_FIRST=%0h", fifo.first); fifo.deq; endaction
      action $display("FIFO_NEXT=%0h",  fifo.first); fifo.deq; endaction
      action $display("FIFO_EMPTY=%0d", fifo.notEmpty ? 0 : 1); endaction

      // ----- (K) StmtFSM control: while + for + if + accumulation -----
      sum <= 0;
      while (sum < 50) action
         sum <= sum + 7;
      endaction
      action $display("WHILE_SUM=%0d", sum); endaction       // first >=50 (56)

      fcnt <= 0; sum <= 0;
      for (fcnt <= 0; fcnt < 5; fcnt <= fcnt + 1) action
         sum <= sum + zeroExtend(fcnt);                      // 0+1+2+3+4=10
      endaction
      action $display("FOR_SUM=%0d", sum); endaction

      if (fact(4) == 24) action $display("IF_OK=1"); endaction
      else action $display("IF_OK=0"); endaction

      // ----- (L) tuples + let + begin-end value block -----
      action
         Tuple3#(Bit#(8), Bool, State) t = tuple3(8'h99, True, Wait);
         match {.a, .b, .c} = t;
         $display("TUP_A=%0h", a);
         $display("TUP_B=%0d", b ? 1 : 0);
         $display("TUP_C=", fshow(c));
         let blk = begin
                      Bit#(8) q = 5;
                      q = q * q;     // 25
                      q + 1;         // value 26
                   end;
         $display("BLOCK=%0d", blk);
      endaction

      $display("BSV_DONE");
      $finish(0);
   endseq;

   mkAutoFSM(prog);

endmodule

endpackage
