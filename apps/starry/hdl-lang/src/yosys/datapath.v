module datapath #(parameter W=8, parameter DEPTH=16) (
   input clk, input rst, input start,
   input [W-1:0] a, input [W-1:0] b, input [2:0] op,
   input [3:0] waddr, input [3:0] raddr, input we,
   output [W-1:0] rdata, output [1:0] phase, output busy, output zero );
   wire [W-1:0] aluy; wire aluz;
   reg [W-1:0] mem [0:DEPTH-1]; reg [W-1:0] rd_r; reg [W-1:0] acc;
   alu #(.W(W)) u_alu (.a(a), .b(b), .op(op), .y(aluy), .zero(aluz));
   ctrl u_ctrl (.clk(clk), .rst(rst), .start(start), .done_in(aluz), .phase(phase), .busy(busy));
   always @(posedge clk) begin
      if (rst) acc <= {W{1'b0}}; else acc <= acc + aluy;
      if (we) mem[waddr] <= aluy;
      rd_r <= mem[raddr];
   end
   assign rdata = rd_r ^ acc;
   assign zero  = aluz;
endmodule
