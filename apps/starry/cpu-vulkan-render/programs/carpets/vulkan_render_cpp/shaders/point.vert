#version 450
layout(location=0) in vec2 p;
void main(){ gl_Position=vec4(p,0.0,1.0); gl_PointSize=1.0; }
