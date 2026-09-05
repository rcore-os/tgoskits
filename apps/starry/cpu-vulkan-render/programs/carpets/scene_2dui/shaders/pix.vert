#version 450
layout(location=0) in vec2 p;
layout(push_constant) uniform PC { vec2 vp; vec4 col; vec4 box; float rad; } pc;
void main(){ vec2 n=(p/pc.vp)*2.0-1.0; gl_Position=vec4(n,0.0,1.0); }
