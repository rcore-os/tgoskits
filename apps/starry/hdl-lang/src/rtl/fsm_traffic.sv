// =============================================================================
// fsm_traffic.sv -- Moore traffic-light FSM (3 states, timed).
//   Demonstrates: enum-typed state register, two-process FSM (seq + comb),
//   always_ff + always_comb, case on enum, default, unique case, parameter
//   dwell times, registered outputs derived from state.
// IEEE 1800-2017: Cl.6.19 (enums), Cl.12.5 (case).
// =============================================================================
module fsm_traffic
  import hdl_pkg::*;
#(
  parameter int unsigned T_RED    = 3,
  parameter int unsigned T_GREEN  = 3,
  parameter int unsigned T_YELLOW = 2
) (
  input  logic   clk,
  input  logic   rst,
  output light_e state,
  output logic   red,
  output logic   green,
  output logic   yellow
);

  light_e       cur, nxt;
  logic [3:0]   timer;
  logic [3:0]   nxt_timer;

  // sequential state + timer
  always_ff @(posedge clk or posedge rst) begin
    if (rst) begin
      cur   <= S_RED;
      timer <= '0;
    end else begin
      cur   <= nxt;
      timer <= nxt_timer;
    end
  end

  // next-state / timer combinational logic
  always_comb begin
    nxt       = cur;
    nxt_timer = timer + 1'b1;
    unique case (cur)
      S_RED:    if (timer >= T_RED   - 1) begin nxt = S_GREEN;  nxt_timer = '0; end
      S_GREEN:  if (timer >= T_GREEN  - 1) begin nxt = S_YELLOW; nxt_timer = '0; end
      S_YELLOW: if (timer >= T_YELLOW - 1) begin nxt = S_RED;    nxt_timer = '0; end
      default:  begin nxt = S_RED; nxt_timer = '0; end
    endcase
  end

  // Moore outputs (pure function of current state)
  always_comb begin
    red    = (cur == S_RED);
    green  = (cur == S_GREEN);
    yellow = (cur == S_YELLOW);
  end

  assign state = cur;

endmodule : fsm_traffic
