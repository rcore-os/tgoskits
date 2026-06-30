module ctrl ( input clk, input rst, input start, input done_in,
   output reg [1:0] phase, output busy );
   localparam IDLE=2'd0, FETCH=2'd1, EXEC=2'd2, WB=2'd3;
   always @(posedge clk) begin
      if (rst) phase <= IDLE;
      else case (phase)
         IDLE:  phase <= start ? FETCH : IDLE;
         FETCH: phase <= EXEC;
         EXEC:  phase <= done_in ? WB : EXEC;
         WB:    phase <= IDLE;
         default: phase <= IDLE;
      endcase
   end
   assign busy = (phase != IDLE);
endmodule
