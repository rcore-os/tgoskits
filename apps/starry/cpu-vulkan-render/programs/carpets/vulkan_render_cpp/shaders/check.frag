#version 450
layout(location=0) out vec4 o;
void main(){ ivec2 c=ivec2(gl_FragCoord.xy); bool e=(((c.x>>3)+(c.y>>3))&1)==0; o=e?vec4(1.0):vec4(0.0,0.0,0.0,1.0); }
