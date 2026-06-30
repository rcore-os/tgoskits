module alu #(parameter W = 8) (
   input  [W-1:0] a, input  [W-1:0] b, input  [2:0] op,
   output reg [W-1:0] y, output zero );
   always @* begin
      case (op)
         3'd0: y = a + b;   3'd1: y = a - b;
         3'd2: y = a & b;   3'd3: y = a | b;
         3'd4: y = a ^ b;   3'd5: y = a << 1;
         3'd6: y = a >> 1;
         3'd7: y = (a < b) ? {{(W-1){1'b0}},1'b1} : {W{1'b0}};
         default: y = {W{1'b0}};
      endcase
   end
   assign zero = (y == {W{1'b0}});
endmodule
