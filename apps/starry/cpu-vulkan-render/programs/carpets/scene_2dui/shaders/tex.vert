#version 450
layout(location=0) in vec2 p;
layout(location=1) in vec2 t;
layout(location=0) out vec2 uv;
layout(push_constant) uniform PC { vec2 vp; } pc;
void main(){ vec2 n=(p/pc.vp)*2.0-1.0; gl_Position=vec4(n,0.0,1.0); uv=t; }
