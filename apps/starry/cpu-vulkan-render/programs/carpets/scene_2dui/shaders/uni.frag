#version 450
layout(location=0) out vec4 o;
layout(push_constant) uniform PC { vec2 vp; vec4 col; vec4 box; float rad; } pc;
void main(){ o=pc.col; }
