#version 450
layout(location=0) in vec2 p;
layout(location=1) in vec2 t;
layout(location=0) out vec2 uv;
void main(){ gl_Position=vec4(p,0.0,1.0); uv=t; }
