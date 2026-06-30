// =============================================================================
// shifter.sv -- parametrized-width barrel shifter (combinational).
//   Demonstrates: parameter width, logarithmic generate-for shift network,
//   genvar, generate/endgenerate, conditional rotate vs shift, arithmetic vs
//   logical, named generate blocks, $clog2.
// IEEE 1800-2017: Cl.27 (generate), Cl.11.4.10 (shift operators).
// =============================================================================
module shifter #(
  parameter int unsigned W = 8
) (
  input  logic [W-1:0]          data,
  input  logic [$clog2(W)-1:0]  amt,
  input  logic                  left,    // 1 = left, 0 = right
  input  logic                  arith,   // 1 = arithmetic right shift (sign fill)
  input  logic                  rotate,  // 1 = rotate instead of shift
  output logic [W-1:0]          result
);

  localparam int unsigned LOGW = $clog2(W);

  // staged barrel network: stage k shifts by 2**k when amt[k] is set
  logic [W-1:0] stage [0:LOGW];

  assign stage[0] = data;

  genvar k;
  generate
    for (k = 0; k < int'(LOGW); k++) begin : g_stage
      localparam int unsigned SH = (1 << k);
      logic [W-1:0] shifted;
      logic [W-1:0] left_v;
      logic [W-1:0] right_v;
      always_comb begin
        if (rotate) begin
          left_v  = (stage[k] << SH) | (stage[k] >> (W - SH));
          right_v = (stage[k] >> SH) | (stage[k] << (W - SH));
        end else if (arith) begin
          left_v  = stage[k] << SH;
          right_v = $signed(stage[k]) >>> SH;
        end else begin
          left_v  = stage[k] << SH;
          right_v = stage[k] >> SH;
        end
        shifted = left ? left_v : right_v;
      end
      assign stage[k+1] = amt[k] ? shifted : stage[k];
    end : g_stage
  endgenerate

  assign result = stage[LOGW];

endmodule : shifter
