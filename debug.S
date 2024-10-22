
apps/libc/build/x86_64/helloworld_c:     file format elf64-x86-64


Disassembly of section .init:

0000000000401000 <_init>:
  401000:	50                   	push   %rax
  401001:	58                   	pop    %rax
  401002:	c3                   	ret    

Disassembly of section .text:

0000000000401010 <exit>:
  401010:	f3 0f 1e fa          	endbr64 
  401014:	55                   	push   %rbp
  401015:	89 fd                	mov    %edi,%ebp
  401017:	e8 54 04 00 00       	call   401470 <__funcs_on_exit>
  40101c:	e8 5f 04 00 00       	call   401480 <__libc_exit_fini>
  401021:	31 c0                	xor    %eax,%eax
  401023:	e8 88 0f 00 00       	call   401fb0 <__stdio_exit>
  401028:	89 ef                	mov    %ebp,%edi
  40102a:	e8 81 08 00 00       	call   4018b0 <_Exit>

000000000040102f <_start>:
  40102f:	48 31 ed             	xor    %rbp,%rbp
  401032:	48 89 e7             	mov    %rsp,%rdi
  401035:	48 8d 35 c4 ef bf ff 	lea    -0x40103c(%rip),%rsi        # 0 <_init-0x401000>
  40103c:	48 83 e4 f0          	and    $0xfffffffffffffff0,%rsp
  401040:	e8 0b 00 00 00       	call   401050 <_start_c>
  401045:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  40104c:	00 00 00 
  40104f:	90                   	nop

0000000000401050 <_start_c>:
  401050:	f3 0f 1e fa          	endbr64 
  401054:	8b 37                	mov    (%rdi),%esi
  401056:	48 8d 57 08          	lea    0x8(%rdi),%rdx
  40105a:	49 c7 c0 b2 22 40 00 	mov    $0x4022b2,%r8
  401061:	45 31 c9             	xor    %r9d,%r9d
  401064:	48 c7 c1 00 10 40 00 	mov    $0x401000,%rcx
  40106b:	48 c7 c7 39 11 40 00 	mov    $0x401139,%rdi
  401072:	e9 b9 03 00 00       	jmp    401430 <__libc_start_main>
  401077:	66 0f 1f 84 00 00 00 	nopw   0x0(%rax,%rax,1)
  40107e:	00 00 

0000000000401080 <deregister_tm_clones>:
  401080:	48 8d 3d a9 40 00 00 	lea    0x40a9(%rip),%rdi        # 405130 <__TMC_END__>
  401087:	48 8d 05 a2 40 00 00 	lea    0x40a2(%rip),%rax        # 405130 <__TMC_END__>
  40108e:	48 39 f8             	cmp    %rdi,%rax
  401091:	74 15                	je     4010a8 <deregister_tm_clones+0x28>
  401093:	48 c7 c0 00 00 00 00 	mov    $0x0,%rax
  40109a:	48 85 c0             	test   %rax,%rax
  40109d:	74 09                	je     4010a8 <deregister_tm_clones+0x28>
  40109f:	ff e0                	jmp    *%rax
  4010a1:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  4010a8:	c3                   	ret    
  4010a9:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

00000000004010b0 <register_tm_clones>:
  4010b0:	48 8d 3d 79 40 00 00 	lea    0x4079(%rip),%rdi        # 405130 <__TMC_END__>
  4010b7:	48 8d 35 72 40 00 00 	lea    0x4072(%rip),%rsi        # 405130 <__TMC_END__>
  4010be:	48 29 fe             	sub    %rdi,%rsi
  4010c1:	48 89 f0             	mov    %rsi,%rax
  4010c4:	48 c1 ee 3f          	shr    $0x3f,%rsi
  4010c8:	48 c1 f8 03          	sar    $0x3,%rax
  4010cc:	48 01 c6             	add    %rax,%rsi
  4010cf:	48 d1 fe             	sar    %rsi
  4010d2:	74 14                	je     4010e8 <register_tm_clones+0x38>
  4010d4:	48 c7 c0 00 00 00 00 	mov    $0x0,%rax
  4010db:	48 85 c0             	test   %rax,%rax
  4010de:	74 08                	je     4010e8 <register_tm_clones+0x38>
  4010e0:	ff e0                	jmp    *%rax
  4010e2:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  4010e8:	c3                   	ret    
  4010e9:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

00000000004010f0 <__do_global_dtors_aux>:
  4010f0:	f3 0f 1e fa          	endbr64 
  4010f4:	80 3d 45 40 00 00 00 	cmpb   $0x0,0x4045(%rip)        # 405140 <completed.0>
  4010fb:	75 2b                	jne    401128 <__do_global_dtors_aux+0x38>
  4010fd:	55                   	push   %rbp
  4010fe:	48 83 3d f2 3e 00 00 	cmpq   $0x0,0x3ef2(%rip)        # 404ff8 <__stdio_ofl_lockptr+0x8>
  401105:	00 
  401106:	48 89 e5             	mov    %rsp,%rbp
  401109:	74 0c                	je     401117 <__do_global_dtors_aux+0x27>
  40110b:	48 8b 3d 0e 3f 00 00 	mov    0x3f0e(%rip),%rdi        # 405020 <__dso_handle>
  401112:	e8 e9 ee bf ff       	call   0 <_init-0x401000>
  401117:	e8 64 ff ff ff       	call   401080 <deregister_tm_clones>
  40111c:	c6 05 1d 40 00 00 01 	movb   $0x1,0x401d(%rip)        # 405140 <completed.0>
  401123:	5d                   	pop    %rbp
  401124:	c3                   	ret    
  401125:	0f 1f 00             	nopl   (%rax)
  401128:	c3                   	ret    
  401129:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

0000000000401130 <frame_dummy>:
  401130:	f3 0f 1e fa          	endbr64 
  401134:	e9 77 ff ff ff       	jmp    4010b0 <register_tm_clones>

0000000000401139 <main>:
  401139:	f3 0f 1e fa          	endbr64 
  40113d:	55                   	push   %rbp
  40113e:	48 89 e5             	mov    %rsp,%rbp
  401141:	48 8d 05 b8 1e 00 00 	lea    0x1eb8(%rip),%rax        # 403000 <_fini+0xd4e>
  401148:	48 89 c7             	mov    %rax,%rdi
  40114b:	e8 70 03 00 00       	call   4014c0 <puts>
  401150:	b8 00 00 00 00       	mov    $0x0,%eax
  401155:	5d                   	pop    %rbp
  401156:	c3                   	ret    
  401157:	66 0f 1f 84 00 00 00 	nopw   0x0(%rax,%rax,1)
  40115e:	00 00 

0000000000401160 <dummy>:
  401160:	f3 0f 1e fa          	endbr64 
  401164:	c3                   	ret    
  401165:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  40116c:	00 00 00 
  40116f:	90                   	nop

0000000000401170 <dummy1>:
  401170:	f3 0f 1e fa          	endbr64 
  401174:	c3                   	ret    
  401175:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  40117c:	00 00 00 
  40117f:	90                   	nop

0000000000401180 <__init_libc>:
  401180:	f3 0f 1e fa          	endbr64 
  401184:	48 81 ec 58 01 00 00 	sub    $0x158,%rsp
  40118b:	48 89 fa             	mov    %rdi,%rdx
  40118e:	31 c0                	xor    %eax,%eax
  401190:	b9 26 00 00 00       	mov    $0x26,%ecx
  401195:	4c 8d 44 24 20       	lea    0x20(%rsp),%r8
  40119a:	48 83 3a 00          	cmpq   $0x0,(%rdx)
  40119e:	4c 89 c7             	mov    %r8,%rdi
  4011a1:	f3 48 ab             	rep stos %rax,%es:(%rdi)
  4011a4:	48 c7 c0 08 56 40 00 	mov    $0x405608,%rax
  4011ab:	48 89 10             	mov    %rdx,(%rax)
  4011ae:	0f 84 cc 01 00 00    	je     401380 <__init_libc+0x200>
  4011b4:	31 c0                	xor    %eax,%eax
  4011b6:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  4011bd:	00 00 00 
  4011c0:	48 89 c1             	mov    %rax,%rcx
  4011c3:	48 83 c0 01          	add    $0x1,%rax
  4011c7:	48 83 3c c2 00       	cmpq   $0x0,(%rdx,%rax,8)
  4011cc:	75 f2                	jne    4011c0 <__init_libc+0x40>
  4011ce:	48 8d 04 cd 10 00 00 	lea    0x10(,%rcx,8),%rax
  4011d5:	00 
  4011d6:	48 01 d0             	add    %rdx,%rax
  4011d9:	48 8b 10             	mov    (%rax),%rdx
  4011dc:	48 89 05 a5 3f 00 00 	mov    %rax,0x3fa5(%rip)        # 405188 <__libc+0x8>
  4011e3:	48 83 c0 08          	add    $0x8,%rax
  4011e7:	48 85 d2             	test   %rdx,%rdx
  4011ea:	0f 84 a0 01 00 00    	je     401390 <__init_libc+0x210>
  4011f0:	48 83 fa 25          	cmp    $0x25,%rdx
  4011f4:	77 08                	ja     4011fe <__init_libc+0x7e>
  4011f6:	48 8b 08             	mov    (%rax),%rcx
  4011f9:	48 89 4c d4 20       	mov    %rcx,0x20(%rsp,%rdx,8)
  4011fe:	48 8b 50 08          	mov    0x8(%rax),%rdx
  401202:	48 83 c0 10          	add    $0x10,%rax
  401206:	48 85 d2             	test   %rdx,%rdx
  401209:	75 e5                	jne    4011f0 <__init_libc+0x70>
  40120b:	48 8b 8c 24 a0 00 00 	mov    0xa0(%rsp),%rcx
  401212:	00 
  401213:	48 8b 84 24 20 01 00 	mov    0x120(%rsp),%rax
  40121a:	00 
  40121b:	48 8b 54 24 50       	mov    0x50(%rsp),%rdx
  401220:	48 89 0d 39 3f 00 00 	mov    %rcx,0x3f39(%rip)        # 405160 <__hwcap>
  401227:	48 85 c0             	test   %rax,%rax
  40122a:	74 07                	je     401233 <__init_libc+0xb3>
  40122c:	48 89 05 15 3f 00 00 	mov    %rax,0x3f15(%rip)        # 405148 <__sysinfo>
  401233:	48 89 15 76 3f 00 00 	mov    %rdx,0x3f76(%rip)        # 4051b0 <__libc+0x30>
  40123a:	48 85 f6             	test   %rsi,%rsi
  40123d:	0f 84 0d 01 00 00    	je     401350 <__init_libc+0x1d0>
  401243:	48 c7 c0 50 51 40 00 	mov    $0x405150,%rax
  40124a:	48 c7 c1 58 51 40 00 	mov    $0x405158,%rcx
  401251:	48 89 30             	mov    %rsi,(%rax)
  401254:	48 89 31             	mov    %rsi,(%rcx)
  401257:	0f b6 16             	movzbl (%rsi),%edx
  40125a:	84 d2                	test   %dl,%dl
  40125c:	74 1d                	je     40127b <__init_libc+0xfb>
  40125e:	48 8d 46 01          	lea    0x1(%rsi),%rax
  401262:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  401268:	80 fa 2f             	cmp    $0x2f,%dl
  40126b:	75 03                	jne    401270 <__init_libc+0xf0>
  40126d:	48 89 01             	mov    %rax,(%rcx)
  401270:	0f b6 10             	movzbl (%rax),%edx
  401273:	48 83 c0 01          	add    $0x1,%rax
  401277:	84 d2                	test   %dl,%dl
  401279:	75 ed                	jne    401268 <__init_libc+0xe8>
  40127b:	4c 89 c7             	mov    %r8,%rdi
  40127e:	e8 0d 04 00 00       	call   401690 <__init_tls>
  401283:	48 8b bc 24 e8 00 00 	mov    0xe8(%rsp),%rdi
  40128a:	00 
  40128b:	e8 10 0c 00 00       	call   401ea0 <__init_ssp>
  401290:	48 8b 84 24 80 00 00 	mov    0x80(%rsp),%rax
  401297:	00 
  401298:	48 39 44 24 78       	cmp    %rax,0x78(%rsp)
  40129d:	0f 84 7d 00 00 00    	je     401320 <__init_libc+0x1a0>
  4012a3:	66 0f ef c0          	pxor   %xmm0,%xmm0
  4012a7:	48 89 e7             	mov    %rsp,%rdi
  4012aa:	b8 07 00 00 00       	mov    $0x7,%eax
  4012af:	31 d2                	xor    %edx,%edx
  4012b1:	48 c7 44 24 10 00 00 	movq   $0x0,0x10(%rsp)
  4012b8:	00 00 
  4012ba:	be 03 00 00 00       	mov    $0x3,%esi
  4012bf:	0f 29 04 24          	movaps %xmm0,(%rsp)
  4012c3:	c7 44 24 10 02 00 00 	movl   $0x2,0x10(%rsp)
  4012ca:	00 
  4012cb:	c7 44 24 08 01 00 00 	movl   $0x1,0x8(%rsp)
  4012d2:	00 
  4012d3:	0f 05                	syscall 
  4012d5:	85 c0                	test   %eax,%eax
  4012d7:	79 01                	jns    4012da <__init_libc+0x15a>
  4012d9:	f4                   	hlt    
  4012da:	48 89 fa             	mov    %rdi,%rdx
  4012dd:	4c 8d 44 24 18       	lea    0x18(%rsp),%r8
  4012e2:	41 b9 02 00 00 00    	mov    $0x2,%r9d
  4012e8:	be 02 80 00 00       	mov    $0x8002,%esi
  4012ed:	48 8d 3d 1a 1d 00 00 	lea    0x1d1a(%rip),%rdi        # 40300e <_fini+0xd5c>
  4012f4:	f6 42 06 20          	testb  $0x20,0x6(%rdx)
  4012f8:	74 0b                	je     401305 <__init_libc+0x185>
  4012fa:	4c 89 c8             	mov    %r9,%rax
  4012fd:	0f 05                	syscall 
  4012ff:	48 85 c0             	test   %rax,%rax
  401302:	79 01                	jns    401305 <__init_libc+0x185>
  401304:	f4                   	hlt    
  401305:	48 83 c2 08          	add    $0x8,%rdx
  401309:	4c 39 c2             	cmp    %r8,%rdx
  40130c:	75 e6                	jne    4012f4 <__init_libc+0x174>
  40130e:	c6 05 6d 3e 00 00 01 	movb   $0x1,0x3e6d(%rip)        # 405182 <__libc+0x2>
  401315:	48 81 c4 58 01 00 00 	add    $0x158,%rsp
  40131c:	c3                   	ret    
  40131d:	0f 1f 00             	nopl   (%rax)
  401320:	48 8b 84 24 90 00 00 	mov    0x90(%rsp),%rax
  401327:	00 
  401328:	48 39 84 24 88 00 00 	cmp    %rax,0x88(%rsp)
  40132f:	00 
  401330:	0f 85 6d ff ff ff    	jne    4012a3 <__init_libc+0x123>
  401336:	48 83 bc 24 d8 00 00 	cmpq   $0x0,0xd8(%rsp)
  40133d:	00 00 
  40133f:	0f 85 5e ff ff ff    	jne    4012a3 <__init_libc+0x123>
  401345:	eb ce                	jmp    401315 <__init_libc+0x195>
  401347:	66 0f 1f 84 00 00 00 	nopw   0x0(%rax,%rax,1)
  40134e:	00 00 
  401350:	48 8b 84 24 18 01 00 	mov    0x118(%rsp),%rax
  401357:	00 
  401358:	48 85 c0             	test   %rax,%rax
  40135b:	75 43                	jne    4013a0 <__init_libc+0x220>
  40135d:	48 c7 c2 50 51 40 00 	mov    $0x405150,%rdx
  401364:	48 8d 05 ac 1c 00 00 	lea    0x1cac(%rip),%rax        # 403017 <_fini+0xd65>
  40136b:	48 89 02             	mov    %rax,(%rdx)
  40136e:	48 c7 c2 58 51 40 00 	mov    $0x405158,%rdx
  401375:	48 89 02             	mov    %rax,(%rdx)
  401378:	e9 fe fe ff ff       	jmp    40127b <__init_libc+0xfb>
  40137d:	0f 1f 00             	nopl   (%rax)
  401380:	b8 08 00 00 00       	mov    $0x8,%eax
  401385:	e9 4c fe ff ff       	jmp    4011d6 <__init_libc+0x56>
  40138a:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  401390:	48 c7 05 c5 3d 00 00 	movq   $0x0,0x3dc5(%rip)        # 405160 <__hwcap>
  401397:	00 00 00 00 
  40139b:	e9 93 fe ff ff       	jmp    401233 <__init_libc+0xb3>
  4013a0:	48 89 c6             	mov    %rax,%rsi
  4013a3:	e9 9b fe ff ff       	jmp    401243 <__init_libc+0xc3>
  4013a8:	0f 1f 84 00 00 00 00 	nopl   0x0(%rax,%rax,1)
  4013af:	00 

00000000004013b0 <__libc_start_init>:
  4013b0:	f3 0f 1e fa          	endbr64 
  4013b4:	55                   	push   %rbp
  4013b5:	53                   	push   %rbx
  4013b6:	48 83 ec 08          	sub    $0x8,%rsp
  4013ba:	e8 41 fc ff ff       	call   401000 <_init>
  4013bf:	48 c7 c3 d8 4f 40 00 	mov    $0x404fd8,%rbx
  4013c6:	48 c7 c5 e0 4f 40 00 	mov    $0x404fe0,%rbp
  4013cd:	48 39 eb             	cmp    %rbp,%rbx
  4013d0:	73 11                	jae    4013e3 <__libc_start_init+0x33>
  4013d2:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  4013d8:	ff 13                	call   *(%rbx)
  4013da:	48 83 c3 08          	add    $0x8,%rbx
  4013de:	48 39 eb             	cmp    %rbp,%rbx
  4013e1:	72 f5                	jb     4013d8 <__libc_start_init+0x28>
  4013e3:	48 83 c4 08          	add    $0x8,%rsp
  4013e7:	5b                   	pop    %rbx
  4013e8:	5d                   	pop    %rbp
  4013e9:	c3                   	ret    
  4013ea:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)

00000000004013f0 <libc_start_main_stage2>:
  4013f0:	f3 0f 1e fa          	endbr64 
  4013f4:	41 55                	push   %r13
  4013f6:	48 63 c6             	movslq %esi,%rax
  4013f9:	41 54                	push   %r12
  4013fb:	4c 8d 6c c2 08       	lea    0x8(%rdx,%rax,8),%r13
  401400:	49 89 d4             	mov    %rdx,%r12
  401403:	55                   	push   %rbp
  401404:	48 89 c5             	mov    %rax,%rbp
  401407:	53                   	push   %rbx
  401408:	48 89 fb             	mov    %rdi,%rbx
  40140b:	48 83 ec 08          	sub    $0x8,%rsp
  40140f:	e8 9c ff ff ff       	call   4013b0 <__libc_start_init>
  401414:	89 ef                	mov    %ebp,%edi
  401416:	4c 89 ea             	mov    %r13,%rdx
  401419:	4c 89 e6             	mov    %r12,%rsi
  40141c:	ff d3                	call   *%rbx
  40141e:	89 c7                	mov    %eax,%edi
  401420:	e8 eb fb ff ff       	call   401010 <exit>
  401425:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  40142c:	00 00 00 
  40142f:	90                   	nop

0000000000401430 <__libc_start_main>:
  401430:	f3 0f 1e fa          	endbr64 
  401434:	41 55                	push   %r13
  401436:	48 63 c6             	movslq %esi,%rax
  401439:	49 89 fd             	mov    %rdi,%r13
  40143c:	41 54                	push   %r12
  40143e:	48 8d 7c c2 08       	lea    0x8(%rdx,%rax,8),%rdi
  401443:	49 89 d4             	mov    %rdx,%r12
  401446:	55                   	push   %rbp
  401447:	48 8b 32             	mov    (%rdx),%rsi
  40144a:	48 89 c5             	mov    %rax,%rbp
  40144d:	e8 2e fd ff ff       	call   401180 <__init_libc>
  401452:	4c 89 e2             	mov    %r12,%rdx
  401455:	89 ee                	mov    %ebp,%esi
  401457:	4c 89 ef             	mov    %r13,%rdi
  40145a:	48 8d 05 8f ff ff ff 	lea    -0x71(%rip),%rax        # 4013f0 <libc_start_main_stage2>
  401461:	5d                   	pop    %rbp
  401462:	41 5c                	pop    %r12
  401464:	41 5d                	pop    %r13
  401466:	ff e0                	jmp    *%rax
  401468:	0f 1f 84 00 00 00 00 	nopl   0x0(%rax,%rax,1)
  40146f:	00 

0000000000401470 <__funcs_on_exit>:
  401470:	f3 0f 1e fa          	endbr64 
  401474:	c3                   	ret    
  401475:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  40147c:	00 00 00 
  40147f:	90                   	nop

0000000000401480 <__libc_exit_fini>:
  401480:	f3 0f 1e fa          	endbr64 
  401484:	55                   	push   %rbp
  401485:	53                   	push   %rbx
  401486:	48 83 ec 08          	sub    $0x8,%rsp
  40148a:	48 c7 c3 e8 4f 40 00 	mov    $0x404fe8,%rbx
  401491:	48 c7 c5 e0 4f 40 00 	mov    $0x404fe0,%rbp
  401498:	48 39 eb             	cmp    %rbp,%rbx
  40149b:	76 10                	jbe    4014ad <__libc_exit_fini+0x2d>
  40149d:	0f 1f 00             	nopl   (%rax)
  4014a0:	48 83 eb 08          	sub    $0x8,%rbx
  4014a4:	31 c0                	xor    %eax,%eax
  4014a6:	ff 13                	call   *(%rbx)
  4014a8:	48 39 eb             	cmp    %rbp,%rbx
  4014ab:	77 f3                	ja     4014a0 <__libc_exit_fini+0x20>
  4014ad:	48 83 c4 08          	add    $0x8,%rsp
  4014b1:	31 c0                	xor    %eax,%eax
  4014b3:	5b                   	pop    %rbx
  4014b4:	5d                   	pop    %rbp
  4014b5:	e9 f8 0d 00 00       	jmp    4022b2 <_fini>
  4014ba:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)

00000000004014c0 <puts>:
  4014c0:	f3 0f 1e fa          	endbr64 
  4014c4:	41 55                	push   %r13
  4014c6:	4c 8d 2d 73 3b 00 00 	lea    0x3b73(%rip),%r13        # 405040 <__stdout_FILE>
  4014cd:	41 54                	push   %r12
  4014cf:	55                   	push   %rbp
  4014d0:	48 89 fd             	mov    %rdi,%rbp
  4014d3:	53                   	push   %rbx
  4014d4:	48 83 ec 08          	sub    $0x8,%rsp
  4014d8:	8b 05 ee 3b 00 00    	mov    0x3bee(%rip),%eax        # 4050cc <__stdout_FILE+0x8c>
  4014de:	85 c0                	test   %eax,%eax
  4014e0:	79 76                	jns    401558 <puts+0x98>
  4014e2:	4c 89 ee             	mov    %r13,%rsi
  4014e5:	41 bc ff ff ff ff    	mov    $0xffffffff,%r12d
  4014eb:	31 db                	xor    %ebx,%ebx
  4014ed:	e8 0e 07 00 00       	call   401c00 <fputs>
  4014f2:	85 c0                	test   %eax,%eax
  4014f4:	78 36                	js     40152c <puts+0x6c>
  4014f6:	83 3d d3 3b 00 00 0a 	cmpl   $0xa,0x3bd3(%rip)        # 4050d0 <__stdout_FILE+0x90>
  4014fd:	74 41                	je     401540 <puts+0x80>
  4014ff:	48 8b 05 62 3b 00 00 	mov    0x3b62(%rip),%rax        # 405068 <__stdout_FILE+0x28>
  401506:	48 3b 05 53 3b 00 00 	cmp    0x3b53(%rip),%rax        # 405060 <__stdout_FILE+0x20>
  40150d:	74 31                	je     401540 <puts+0x80>
  40150f:	48 8d 50 01          	lea    0x1(%rax),%rdx
  401513:	45 31 e4             	xor    %r12d,%r12d
  401516:	48 89 15 4b 3b 00 00 	mov    %rdx,0x3b4b(%rip)        # 405068 <__stdout_FILE+0x28>
  40151d:	c6 00 0a             	movb   $0xa,(%rax)
  401520:	85 db                	test   %ebx,%ebx
  401522:	74 08                	je     40152c <puts+0x6c>
  401524:	4c 89 ef             	mov    %r13,%rdi
  401527:	e8 84 04 00 00       	call   4019b0 <__unlockfile>
  40152c:	48 83 c4 08          	add    $0x8,%rsp
  401530:	44 89 e0             	mov    %r12d,%eax
  401533:	5b                   	pop    %rbx
  401534:	5d                   	pop    %rbp
  401535:	41 5c                	pop    %r12
  401537:	41 5d                	pop    %r13
  401539:	c3                   	ret    
  40153a:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  401540:	be 0a 00 00 00       	mov    $0xa,%esi
  401545:	4c 89 ef             	mov    %r13,%rdi
  401548:	e8 b3 04 00 00       	call   401a00 <__overflow>
  40154d:	c1 f8 1f             	sar    $0x1f,%eax
  401550:	41 89 c4             	mov    %eax,%r12d
  401553:	eb cb                	jmp    401520 <puts+0x60>
  401555:	0f 1f 00             	nopl   (%rax)
  401558:	4c 89 ef             	mov    %r13,%rdi
  40155b:	41 bc ff ff ff ff    	mov    $0xffffffff,%r12d
  401561:	e8 6a 03 00 00       	call   4018d0 <__lockfile>
  401566:	4c 89 ee             	mov    %r13,%rsi
  401569:	48 89 ef             	mov    %rbp,%rdi
  40156c:	89 c3                	mov    %eax,%ebx
  40156e:	e8 8d 06 00 00       	call   401c00 <fputs>
  401573:	85 c0                	test   %eax,%eax
  401575:	78 a9                	js     401520 <puts+0x60>
  401577:	83 3d 52 3b 00 00 0a 	cmpl   $0xa,0x3b52(%rip)        # 4050d0 <__stdout_FILE+0x90>
  40157e:	0f 85 7b ff ff ff    	jne    4014ff <puts+0x3f>
  401584:	eb ba                	jmp    401540 <puts+0x80>
  401586:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  40158d:	00 00 00 

0000000000401590 <__init_tp>:
  401590:	f3 0f 1e fa          	endbr64 
  401594:	53                   	push   %rbx
  401595:	48 89 fb             	mov    %rdi,%rbx
  401598:	48 89 3f             	mov    %rdi,(%rdi)
  40159b:	e8 cf 08 00 00       	call   401e6f <__set_thread_area>
  4015a0:	85 c0                	test   %eax,%eax
  4015a2:	78 54                	js     4015f8 <__init_tp+0x68>
  4015a4:	75 07                	jne    4015ad <__init_tp+0x1d>
  4015a6:	c6 05 d3 3b 00 00 01 	movb   $0x1,0x3bd3(%rip)        # 405180 <__libc>
  4015ad:	c7 43 38 02 00 00 00 	movl   $0x2,0x38(%rbx)
  4015b4:	b8 da 00 00 00       	mov    $0xda,%eax
  4015b9:	48 8d 3d f0 41 00 00 	lea    0x41f0(%rip),%rdi        # 4057b0 <__thread_list_lock>
  4015c0:	0f 05                	syscall 
  4015c2:	89 43 30             	mov    %eax,0x30(%rbx)
  4015c5:	48 8d 05 ec 3b 00 00 	lea    0x3bec(%rip),%rax        # 4051b8 <__libc+0x38>
  4015cc:	48 89 83 a8 00 00 00 	mov    %rax,0xa8(%rbx)
  4015d3:	48 8d 83 88 00 00 00 	lea    0x88(%rbx),%rax
  4015da:	48 89 83 88 00 00 00 	mov    %rax,0x88(%rbx)
  4015e1:	48 8b 05 60 3b 00 00 	mov    0x3b60(%rip),%rax        # 405148 <__sysinfo>
  4015e8:	48 89 5b 10          	mov    %rbx,0x10(%rbx)
  4015ec:	48 89 43 20          	mov    %rax,0x20(%rbx)
  4015f0:	31 c0                	xor    %eax,%eax
  4015f2:	48 89 5b 18          	mov    %rbx,0x18(%rbx)
  4015f6:	5b                   	pop    %rbx
  4015f7:	c3                   	ret    
  4015f8:	b8 ff ff ff ff       	mov    $0xffffffff,%eax
  4015fd:	5b                   	pop    %rbx
  4015fe:	c3                   	ret    
  4015ff:	90                   	nop

0000000000401600 <__copy_tls>:
  401600:	f3 0f 1e fa          	endbr64 
  401604:	41 55                	push   %r13
  401606:	49 89 fd             	mov    %rdi,%r13
  401609:	41 54                	push   %r12
  40160b:	55                   	push   %rbp
  40160c:	53                   	push   %rbx
  40160d:	48 83 ec 08          	sub    $0x8,%rsp
  401611:	48 8b 05 80 3b 00 00 	mov    0x3b80(%rip),%rax        # 405198 <__libc+0x18>
  401618:	4c 8b 25 81 3b 00 00 	mov    0x3b81(%rip),%r12        # 4051a0 <__libc+0x20>
  40161f:	48 8b 1d 6a 3b 00 00 	mov    0x3b6a(%rip),%rbx        # 405190 <__libc+0x10>
  401626:	48 8d 84 07 38 ff ff 	lea    -0xc8(%rdi,%rax,1),%rax
  40162d:	ff 
  40162e:	49 f7 dc             	neg    %r12
  401631:	49 21 c4             	and    %rax,%r12
  401634:	48 85 db             	test   %rbx,%rbx
  401637:	74 32                	je     40166b <__copy_tls+0x6b>
  401639:	48 8d 6f 08          	lea    0x8(%rdi),%rbp
  40163d:	0f 1f 00             	nopl   (%rax)
  401640:	4c 89 e0             	mov    %r12,%rax
  401643:	48 2b 43 28          	sub    0x28(%rbx),%rax
  401647:	48 8b 73 08          	mov    0x8(%rbx),%rsi
  40164b:	4c 89 e7             	mov    %r12,%rdi
  40164e:	48 89 45 00          	mov    %rax,0x0(%rbp)
  401652:	48 8b 53 10          	mov    0x10(%rbx),%rdx
  401656:	48 83 c5 08          	add    $0x8,%rbp
  40165a:	48 2b 7b 28          	sub    0x28(%rbx),%rdi
  40165e:	e8 da 07 00 00       	call   401e3d <memcpy>
  401663:	48 8b 1b             	mov    (%rbx),%rbx
  401666:	48 85 db             	test   %rbx,%rbx
  401669:	75 d5                	jne    401640 <__copy_tls+0x40>
  40166b:	48 8b 05 36 3b 00 00 	mov    0x3b36(%rip),%rax        # 4051a8 <__libc+0x28>
  401672:	49 89 45 00          	mov    %rax,0x0(%r13)
  401676:	4c 89 e0             	mov    %r12,%rax
  401679:	4d 89 6c 24 08       	mov    %r13,0x8(%r12)
  40167e:	48 83 c4 08          	add    $0x8,%rsp
  401682:	5b                   	pop    %rbx
  401683:	5d                   	pop    %rbp
  401684:	41 5c                	pop    %r12
  401686:	41 5d                	pop    %r13
  401688:	c3                   	ret    
  401689:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

0000000000401690 <__init_tls>:
  401690:	f3 0f 1e fa          	endbr64 
  401694:	53                   	push   %rbx
  401695:	48 8b 4f 28          	mov    0x28(%rdi),%rcx
  401699:	4c 8b 5f 18          	mov    0x18(%rdi),%r11
  40169d:	48 85 c9             	test   %rcx,%rcx
  4016a0:	0f 84 ea 01 00 00    	je     401890 <__init_tls+0x200>
  4016a6:	44 8b 05 7f 3a 00 00 	mov    0x3a7f(%rip),%r8d        # 40512c <__default_stacksize>
  4016ad:	48 8b 7f 20          	mov    0x20(%rdi),%rdi
  4016b1:	4c 89 d8             	mov    %r11,%rax
  4016b4:	31 db                	xor    %ebx,%ebx
  4016b6:	31 f6                	xor    %esi,%esi
  4016b8:	45 31 c9             	xor    %r9d,%r9d
  4016bb:	eb 21                	jmp    4016de <__init_tls+0x4e>
  4016bd:	0f 1f 00             	nopl   (%rax)
  4016c0:	83 fa 02             	cmp    $0x2,%edx
  4016c3:	0f 84 a7 01 00 00    	je     401870 <__init_tls+0x1e0>
  4016c9:	83 fa 07             	cmp    $0x7,%edx
  4016cc:	0f 85 68 01 00 00    	jne    40183a <__init_tls+0x1aa>
  4016d2:	49 89 c1             	mov    %rax,%r9
  4016d5:	48 01 f8             	add    %rdi,%rax
  4016d8:	48 83 e9 01          	sub    $0x1,%rcx
  4016dc:	74 17                	je     4016f5 <__init_tls+0x65>
  4016de:	8b 10                	mov    (%rax),%edx
  4016e0:	83 fa 06             	cmp    $0x6,%edx
  4016e3:	75 db                	jne    4016c0 <__init_tls+0x30>
  4016e5:	4c 89 de             	mov    %r11,%rsi
  4016e8:	48 2b 70 10          	sub    0x10(%rax),%rsi
  4016ec:	48 01 f8             	add    %rdi,%rax
  4016ef:	48 83 e9 01          	sub    $0x1,%rcx
  4016f3:	75 e9                	jne    4016de <__init_tls+0x4e>
  4016f5:	84 db                	test   %bl,%bl
  4016f7:	74 07                	je     401700 <__init_tls+0x70>
  4016f9:	44 89 05 2c 3a 00 00 	mov    %r8d,0x3a2c(%rip)        # 40512c <__default_stacksize>
  401700:	4d 85 c9             	test   %r9,%r9
  401703:	0f 84 87 01 00 00    	je     401890 <__init_tls+0x200>
  401709:	49 8b 41 20          	mov    0x20(%r9),%rax
  40170d:	49 03 71 10          	add    0x10(%r9),%rsi
  401711:	48 8d 0d 08 3f 00 00 	lea    0x3f08(%rip),%rcx        # 405620 <main_tls>
  401718:	49 8b 51 28          	mov    0x28(%r9),%rdx
  40171c:	48 89 35 05 3f 00 00 	mov    %rsi,0x3f05(%rip)        # 405628 <main_tls+0x8>
  401723:	48 89 05 06 3f 00 00 	mov    %rax,0x3f06(%rip)        # 405630 <main_tls+0x10>
  40172a:	49 8b 41 30          	mov    0x30(%r9),%rax
  40172e:	48 c7 05 6f 3a 00 00 	movq   $0x1,0x3a6f(%rip)        # 4051a8 <__libc+0x28>
  401735:	01 00 00 00 
  401739:	48 89 05 00 3f 00 00 	mov    %rax,0x3f00(%rip)        # 405640 <main_tls+0x20>
  401740:	48 89 0d 49 3a 00 00 	mov    %rcx,0x3a49(%rip)        # 405190 <__libc+0x10>
  401747:	48 01 d6             	add    %rdx,%rsi
  40174a:	48 8d 48 ff          	lea    -0x1(%rax),%rcx
  40174e:	48 f7 de             	neg    %rsi
  401751:	48 21 ce             	and    %rcx,%rsi
  401754:	48 01 d6             	add    %rdx,%rsi
  401757:	48 8d 90 df 00 00 00 	lea    0xdf(%rax),%rdx
  40175e:	48 89 35 d3 3e 00 00 	mov    %rsi,0x3ed3(%rip)        # 405638 <main_tls+0x18>
  401765:	48 89 35 dc 3e 00 00 	mov    %rsi,0x3edc(%rip)        # 405648 <main_tls+0x28>
  40176c:	48 83 f8 07          	cmp    $0x7,%rax
  401770:	77 15                	ja     401787 <__init_tls+0xf7>
  401772:	48 c7 05 c3 3e 00 00 	movq   $0x8,0x3ec3(%rip)        # 405640 <main_tls+0x20>
  401779:	08 00 00 00 
  40177d:	ba e7 00 00 00       	mov    $0xe7,%edx
  401782:	b8 08 00 00 00       	mov    $0x8,%eax
  401787:	48 01 d6             	add    %rdx,%rsi
  40178a:	48 89 05 0f 3a 00 00 	mov    %rax,0x3a0f(%rip)        # 4051a0 <__libc+0x20>
  401791:	48 8d 3d c8 3e 00 00 	lea    0x3ec8(%rip),%rdi        # 405660 <builtin_tls>
  401798:	48 83 e6 f8          	and    $0xfffffffffffffff8,%rsi
  40179c:	48 89 35 f5 39 00 00 	mov    %rsi,0x39f5(%rip)        # 405198 <__libc+0x18>
  4017a3:	48 81 fe 50 01 00 00 	cmp    $0x150,%rsi
  4017aa:	76 21                	jbe    4017cd <__init_tls+0x13d>
  4017ac:	41 ba 22 00 00 00    	mov    $0x22,%r10d
  4017b2:	45 31 c9             	xor    %r9d,%r9d
  4017b5:	b8 09 00 00 00       	mov    $0x9,%eax
  4017ba:	31 ff                	xor    %edi,%edi
  4017bc:	49 c7 c0 ff ff ff ff 	mov    $0xffffffffffffffff,%r8
  4017c3:	ba 03 00 00 00       	mov    $0x3,%edx
  4017c8:	0f 05                	syscall 
  4017ca:	48 89 c7             	mov    %rax,%rdi
  4017cd:	e8 2e fe ff ff       	call   401600 <__copy_tls>
  4017d2:	48 89 00             	mov    %rax,(%rax)
  4017d5:	48 89 c7             	mov    %rax,%rdi
  4017d8:	48 89 c3             	mov    %rax,%rbx
  4017db:	e8 8f 06 00 00       	call   401e6f <__set_thread_area>
  4017e0:	85 c0                	test   %eax,%eax
  4017e2:	0f 88 a4 00 00 00    	js     40188c <__init_tls+0x1fc>
  4017e8:	75 07                	jne    4017f1 <__init_tls+0x161>
  4017ea:	c6 05 8f 39 00 00 01 	movb   $0x1,0x398f(%rip)        # 405180 <__libc>
  4017f1:	c7 43 38 02 00 00 00 	movl   $0x2,0x38(%rbx)
  4017f8:	b8 da 00 00 00       	mov    $0xda,%eax
  4017fd:	48 8d 3d ac 3f 00 00 	lea    0x3fac(%rip),%rdi        # 4057b0 <__thread_list_lock>
  401804:	0f 05                	syscall 
  401806:	89 43 30             	mov    %eax,0x30(%rbx)
  401809:	48 8d 05 a8 39 00 00 	lea    0x39a8(%rip),%rax        # 4051b8 <__libc+0x38>
  401810:	48 89 83 a8 00 00 00 	mov    %rax,0xa8(%rbx)
  401817:	48 8d 83 88 00 00 00 	lea    0x88(%rbx),%rax
  40181e:	48 89 83 88 00 00 00 	mov    %rax,0x88(%rbx)
  401825:	48 8b 05 1c 39 00 00 	mov    0x391c(%rip),%rax        # 405148 <__sysinfo>
  40182c:	48 89 5b 10          	mov    %rbx,0x10(%rbx)
  401830:	48 89 43 20          	mov    %rax,0x20(%rbx)
  401834:	48 89 5b 18          	mov    %rbx,0x18(%rbx)
  401838:	5b                   	pop    %rbx
  401839:	c3                   	ret    
  40183a:	81 fa 51 e5 74 64    	cmp    $0x6474e551,%edx
  401840:	0f 85 8f fe ff ff    	jne    4016d5 <__init_tls+0x45>
  401846:	48 8b 50 28          	mov    0x28(%rax),%rdx
  40184a:	45 89 c2             	mov    %r8d,%r10d
  40184d:	4c 39 d2             	cmp    %r10,%rdx
  401850:	0f 86 7f fe ff ff    	jbe    4016d5 <__init_tls+0x45>
  401856:	41 b8 00 00 80 00    	mov    $0x800000,%r8d
  40185c:	bb 01 00 00 00       	mov    $0x1,%ebx
  401861:	4c 39 c2             	cmp    %r8,%rdx
  401864:	4c 0f 46 c2          	cmovbe %rdx,%r8
  401868:	e9 68 fe ff ff       	jmp    4016d5 <__init_tls+0x45>
  40186d:	0f 1f 00             	nopl   (%rax)
  401870:	48 c7 c2 00 00 00 00 	mov    $0x0,%rdx
  401877:	48 85 d2             	test   %rdx,%rdx
  40187a:	0f 84 55 fe ff ff    	je     4016d5 <__init_tls+0x45>
  401880:	48 89 d6             	mov    %rdx,%rsi
  401883:	48 2b 70 10          	sub    0x10(%rax),%rsi
  401887:	e9 49 fe ff ff       	jmp    4016d5 <__init_tls+0x45>
  40188c:	f4                   	hlt    
  40188d:	5b                   	pop    %rbx
  40188e:	c3                   	ret    
  40188f:	90                   	nop
  401890:	48 8b 15 a1 3d 00 00 	mov    0x3da1(%rip),%rdx        # 405638 <main_tls+0x18>
  401897:	48 8b 35 8a 3d 00 00 	mov    0x3d8a(%rip),%rsi        # 405628 <main_tls+0x8>
  40189e:	48 8b 05 9b 3d 00 00 	mov    0x3d9b(%rip),%rax        # 405640 <main_tls+0x20>
  4018a5:	e9 9d fe ff ff       	jmp    401747 <__init_tls+0xb7>
  4018aa:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)

00000000004018b0 <_Exit>:
  4018b0:	f3 0f 1e fa          	endbr64 
  4018b4:	48 63 ff             	movslq %edi,%rdi
  4018b7:	b8 e7 00 00 00       	mov    $0xe7,%eax
  4018bc:	0f 05                	syscall 
  4018be:	ba 3c 00 00 00       	mov    $0x3c,%edx
  4018c3:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
  4018c8:	48 89 d0             	mov    %rdx,%rax
  4018cb:	0f 05                	syscall 
  4018cd:	eb f9                	jmp    4018c8 <_Exit+0x18>
  4018cf:	90                   	nop

00000000004018d0 <__lockfile>:
  4018d0:	f3 0f 1e fa          	endbr64 
  4018d4:	8b 87 8c 00 00 00    	mov    0x8c(%rdi),%eax
  4018da:	45 31 c0             	xor    %r8d,%r8d
  4018dd:	64 48 8b 0c 25 00 00 	mov    %fs:0x0,%rcx
  4018e4:	00 00 
  4018e6:	44 8b 49 30          	mov    0x30(%rcx),%r9d
  4018ea:	25 ff ff ff bf       	and    $0xbfffffff,%eax
  4018ef:	44 39 c8             	cmp    %r9d,%eax
  4018f2:	0f 84 ae 00 00 00    	je     4019a6 <__lockfile+0xd6>
  4018f8:	48 89 fa             	mov    %rdi,%rdx
  4018fb:	44 89 c0             	mov    %r8d,%eax
  4018fe:	48 8d bf 8c 00 00 00 	lea    0x8c(%rdi),%rdi
  401905:	f0 44 0f b1 8a 8c 00 	lock cmpxchg %r9d,0x8c(%rdx)
  40190c:	00 00 
  40190e:	85 c0                	test   %eax,%eax
  401910:	0f 84 8a 00 00 00    	je     4019a0 <__lockfile+0xd0>
  401916:	41 81 c9 00 00 00 40 	or     $0x40000000,%r9d
  40191d:	44 89 c0             	mov    %r8d,%eax
  401920:	f0 44 0f b1 8a 8c 00 	lock cmpxchg %r9d,0x8c(%rdx)
  401927:	00 00 
  401929:	89 c1                	mov    %eax,%ecx
  40192b:	85 c0                	test   %eax,%eax
  40192d:	74 71                	je     4019a0 <__lockfile+0xd0>
  40192f:	55                   	push   %rbp
  401930:	45 31 c0             	xor    %r8d,%r8d
  401933:	bd 80 00 00 00       	mov    $0x80,%ebp
  401938:	53                   	push   %rbx
  401939:	bb ca 00 00 00       	mov    $0xca,%ebx
  40193e:	eb 18                	jmp    401958 <__lockfile+0x88>
  401940:	89 c8                	mov    %ecx,%eax
  401942:	f0 0f b1 17          	lock cmpxchg %edx,(%rdi)
  401946:	39 c8                	cmp    %ecx,%eax
  401948:	74 1e                	je     401968 <__lockfile+0x98>
  40194a:	44 89 c0             	mov    %r8d,%eax
  40194d:	f0 44 0f b1 0f       	lock cmpxchg %r9d,(%rdi)
  401952:	89 c1                	mov    %eax,%ecx
  401954:	85 c0                	test   %eax,%eax
  401956:	74 38                	je     401990 <__lockfile+0xc0>
  401958:	89 ca                	mov    %ecx,%edx
  40195a:	81 ca 00 00 00 40    	or     $0x40000000,%edx
  401960:	f7 c1 00 00 00 40    	test   $0x40000000,%ecx
  401966:	74 d8                	je     401940 <__lockfile+0x70>
  401968:	48 63 d2             	movslq %edx,%rdx
  40196b:	45 31 d2             	xor    %r10d,%r10d
  40196e:	48 89 d8             	mov    %rbx,%rax
  401971:	48 89 ee             	mov    %rbp,%rsi
  401974:	0f 05                	syscall 
  401976:	48 83 f8 da          	cmp    $0xffffffffffffffda,%rax
  40197a:	75 ce                	jne    40194a <__lockfile+0x7a>
  40197c:	45 31 d2             	xor    %r10d,%r10d
  40197f:	31 f6                	xor    %esi,%esi
  401981:	48 89 d8             	mov    %rbx,%rax
  401984:	0f 05                	syscall 
  401986:	eb c2                	jmp    40194a <__lockfile+0x7a>
  401988:	0f 1f 84 00 00 00 00 	nopl   0x0(%rax,%rax,1)
  40198f:	00 
  401990:	b8 01 00 00 00       	mov    $0x1,%eax
  401995:	5b                   	pop    %rbx
  401996:	5d                   	pop    %rbp
  401997:	c3                   	ret    
  401998:	0f 1f 84 00 00 00 00 	nopl   0x0(%rax,%rax,1)
  40199f:	00 
  4019a0:	41 b8 01 00 00 00    	mov    $0x1,%r8d
  4019a6:	44 89 c0             	mov    %r8d,%eax
  4019a9:	c3                   	ret    
  4019aa:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)

00000000004019b0 <__unlockfile>:
  4019b0:	f3 0f 1e fa          	endbr64 
  4019b4:	48 89 fa             	mov    %rdi,%rdx
  4019b7:	31 c0                	xor    %eax,%eax
  4019b9:	48 8d bf 8c 00 00 00 	lea    0x8c(%rdi),%rdi
  4019c0:	87 82 8c 00 00 00    	xchg   %eax,0x8c(%rdx)
  4019c6:	a9 00 00 00 40       	test   $0x40000000,%eax
  4019cb:	74 1b                	je     4019e8 <__unlockfile+0x38>
  4019cd:	41 b8 ca 00 00 00    	mov    $0xca,%r8d
  4019d3:	ba 01 00 00 00       	mov    $0x1,%edx
  4019d8:	be 81 00 00 00       	mov    $0x81,%esi
  4019dd:	4c 89 c0             	mov    %r8,%rax
  4019e0:	0f 05                	syscall 
  4019e2:	48 83 f8 da          	cmp    $0xffffffffffffffda,%rax
  4019e6:	74 08                	je     4019f0 <__unlockfile+0x40>
  4019e8:	c3                   	ret    
  4019e9:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  4019f0:	4c 89 c0             	mov    %r8,%rax
  4019f3:	48 89 d6             	mov    %rdx,%rsi
  4019f6:	0f 05                	syscall 
  4019f8:	c3                   	ret    
  4019f9:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

0000000000401a00 <__overflow>:
  401a00:	f3 0f 1e fa          	endbr64 
  401a04:	53                   	push   %rbx
  401a05:	48 89 fb             	mov    %rdi,%rbx
  401a08:	48 83 ec 10          	sub    $0x10,%rsp
  401a0c:	64 48 8b 04 25 28 00 	mov    %fs:0x28,%rax
  401a13:	00 00 
  401a15:	48 89 44 24 08       	mov    %rax,0x8(%rsp)
  401a1a:	31 c0                	xor    %eax,%eax
  401a1c:	48 8b 47 20          	mov    0x20(%rdi),%rax
  401a20:	40 88 74 24 07       	mov    %sil,0x7(%rsp)
  401a25:	48 85 c0             	test   %rax,%rax
  401a28:	74 5e                	je     401a88 <__overflow+0x88>
  401a2a:	48 8b 53 28          	mov    0x28(%rbx),%rdx
  401a2e:	48 39 c2             	cmp    %rax,%rdx
  401a31:	74 0d                	je     401a40 <__overflow+0x40>
  401a33:	0f b6 44 24 07       	movzbl 0x7(%rsp),%eax
  401a38:	3b 83 90 00 00 00    	cmp    0x90(%rbx),%eax
  401a3e:	75 38                	jne    401a78 <__overflow+0x78>
  401a40:	48 8d 74 24 07       	lea    0x7(%rsp),%rsi
  401a45:	ba 01 00 00 00       	mov    $0x1,%edx
  401a4a:	48 89 df             	mov    %rbx,%rdi
  401a4d:	ff 53 48             	call   *0x48(%rbx)
  401a50:	48 83 f8 01          	cmp    $0x1,%rax
  401a54:	75 4a                	jne    401aa0 <__overflow+0xa0>
  401a56:	0f b6 44 24 07       	movzbl 0x7(%rsp),%eax
  401a5b:	48 8b 54 24 08       	mov    0x8(%rsp),%rdx
  401a60:	64 48 2b 14 25 28 00 	sub    %fs:0x28,%rdx
  401a67:	00 00 
  401a69:	75 3c                	jne    401aa7 <__overflow+0xa7>
  401a6b:	48 83 c4 10          	add    $0x10,%rsp
  401a6f:	5b                   	pop    %rbx
  401a70:	c3                   	ret    
  401a71:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  401a78:	48 8d 72 01          	lea    0x1(%rdx),%rsi
  401a7c:	48 89 73 28          	mov    %rsi,0x28(%rbx)
  401a80:	88 02                	mov    %al,(%rdx)
  401a82:	eb d7                	jmp    401a5b <__overflow+0x5b>
  401a84:	0f 1f 40 00          	nopl   0x0(%rax)
  401a88:	e8 03 01 00 00       	call   401b90 <__towrite>
  401a8d:	85 c0                	test   %eax,%eax
  401a8f:	75 0f                	jne    401aa0 <__overflow+0xa0>
  401a91:	48 8b 43 20          	mov    0x20(%rbx),%rax
  401a95:	eb 93                	jmp    401a2a <__overflow+0x2a>
  401a97:	66 0f 1f 84 00 00 00 	nopw   0x0(%rax,%rax,1)
  401a9e:	00 00 
  401aa0:	b8 ff ff ff ff       	mov    $0xffffffff,%eax
  401aa5:	eb b4                	jmp    401a5b <__overflow+0x5b>
  401aa7:	e8 44 04 00 00       	call   401ef0 <__stack_chk_fail>
  401aac:	0f 1f 40 00          	nopl   0x0(%rax)

0000000000401ab0 <__aio_close>:
  401ab0:	f3 0f 1e fa          	endbr64 
  401ab4:	89 f8                	mov    %edi,%eax
  401ab6:	c3                   	ret    
  401ab7:	66 0f 1f 84 00 00 00 	nopw   0x0(%rax,%rax,1)
  401abe:	00 00 

0000000000401ac0 <__stdio_close>:
  401ac0:	f3 0f 1e fa          	endbr64 
  401ac4:	48 83 ec 08          	sub    $0x8,%rsp
  401ac8:	8b 7f 78             	mov    0x78(%rdi),%edi
  401acb:	e8 e0 ff ff ff       	call   401ab0 <__aio_close>
  401ad0:	48 63 f8             	movslq %eax,%rdi
  401ad3:	b8 03 00 00 00       	mov    $0x3,%eax
  401ad8:	0f 05                	syscall 
  401ada:	48 89 c7             	mov    %rax,%rdi
  401add:	e8 1e 04 00 00       	call   401f00 <__syscall_ret>
  401ae2:	48 83 c4 08          	add    $0x8,%rsp
  401ae6:	c3                   	ret    
  401ae7:	66 0f 1f 84 00 00 00 	nopw   0x0(%rax,%rax,1)
  401aee:	00 00 

0000000000401af0 <__stdio_seek>:
  401af0:	f3 0f 1e fa          	endbr64 
  401af4:	8b 7f 78             	mov    0x78(%rdi),%edi
  401af7:	e9 84 03 00 00       	jmp    401e80 <__lseek>
  401afc:	0f 1f 40 00          	nopl   0x0(%rax)

0000000000401b00 <__stdout_write>:
  401b00:	f3 0f 1e fa          	endbr64 
  401b04:	48 83 ec 18          	sub    $0x18,%rsp
  401b08:	49 89 f8             	mov    %rdi,%r8
  401b0b:	49 89 f1             	mov    %rsi,%r9
  401b0e:	49 89 d2             	mov    %rdx,%r10
  401b11:	64 48 8b 04 25 28 00 	mov    %fs:0x28,%rax
  401b18:	00 00 
  401b1a:	48 89 44 24 08       	mov    %rax,0x8(%rsp)
  401b1f:	48 8d 05 da 04 00 00 	lea    0x4da(%rip),%rax        # 402000 <__stdio_write>
  401b26:	48 89 47 48          	mov    %rax,0x48(%rdi)
  401b2a:	f6 07 40             	testb  $0x40,(%rdi)
  401b2d:	75 18                	jne    401b47 <__stdout_write+0x47>
  401b2f:	48 63 7f 78          	movslq 0x78(%rdi),%rdi
  401b33:	48 89 e2             	mov    %rsp,%rdx
  401b36:	b8 10 00 00 00       	mov    $0x10,%eax
  401b3b:	be 13 54 00 00       	mov    $0x5413,%esi
  401b40:	0f 05                	syscall 
  401b42:	48 85 c0             	test   %rax,%rax
  401b45:	75 29                	jne    401b70 <__stdout_write+0x70>
  401b47:	4c 89 d2             	mov    %r10,%rdx
  401b4a:	4c 89 ce             	mov    %r9,%rsi
  401b4d:	4c 89 c7             	mov    %r8,%rdi
  401b50:	e8 ab 04 00 00       	call   402000 <__stdio_write>
  401b55:	48 8b 54 24 08       	mov    0x8(%rsp),%rdx
  401b5a:	64 48 2b 14 25 28 00 	sub    %fs:0x28,%rdx
  401b61:	00 00 
  401b63:	75 18                	jne    401b7d <__stdout_write+0x7d>
  401b65:	48 83 c4 18          	add    $0x18,%rsp
  401b69:	c3                   	ret    
  401b6a:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  401b70:	41 c7 80 90 00 00 00 	movl   $0xffffffff,0x90(%r8)
  401b77:	ff ff ff ff 
  401b7b:	eb ca                	jmp    401b47 <__stdout_write+0x47>
  401b7d:	e8 6e 03 00 00       	call   401ef0 <__stack_chk_fail>
  401b82:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  401b89:	00 00 00 
  401b8c:	0f 1f 40 00          	nopl   0x0(%rax)

0000000000401b90 <__towrite>:
  401b90:	f3 0f 1e fa          	endbr64 
  401b94:	8b 97 88 00 00 00    	mov    0x88(%rdi),%edx
  401b9a:	8d 42 ff             	lea    -0x1(%rdx),%eax
  401b9d:	09 d0                	or     %edx,%eax
  401b9f:	89 87 88 00 00 00    	mov    %eax,0x88(%rdi)
  401ba5:	8b 07                	mov    (%rdi),%eax
  401ba7:	a8 08                	test   $0x8,%al
  401ba9:	75 2d                	jne    401bd8 <__towrite+0x48>
  401bab:	48 8b 47 58          	mov    0x58(%rdi),%rax
  401baf:	48 c7 47 10 00 00 00 	movq   $0x0,0x10(%rdi)
  401bb6:	00 
  401bb7:	48 c7 47 08 00 00 00 	movq   $0x0,0x8(%rdi)
  401bbe:	00 
  401bbf:	48 89 47 38          	mov    %rax,0x38(%rdi)
  401bc3:	48 89 47 28          	mov    %rax,0x28(%rdi)
  401bc7:	48 03 47 60          	add    0x60(%rdi),%rax
  401bcb:	48 89 47 20          	mov    %rax,0x20(%rdi)
  401bcf:	31 c0                	xor    %eax,%eax
  401bd1:	c3                   	ret    
  401bd2:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  401bd8:	83 c8 20             	or     $0x20,%eax
  401bdb:	89 07                	mov    %eax,(%rdi)
  401bdd:	b8 ff ff ff ff       	mov    $0xffffffff,%eax
  401be2:	c3                   	ret    
  401be3:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  401bea:	00 00 00 
  401bed:	0f 1f 00             	nopl   (%rax)

0000000000401bf0 <__towrite_needs_stdio_exit>:
  401bf0:	f3 0f 1e fa          	endbr64 
  401bf4:	e9 b7 03 00 00       	jmp    401fb0 <__stdio_exit>
  401bf9:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

0000000000401c00 <fputs>:
  401c00:	f3 0f 1e fa          	endbr64 
  401c04:	41 54                	push   %r12
  401c06:	49 89 f4             	mov    %rsi,%r12
  401c09:	55                   	push   %rbp
  401c0a:	48 89 fd             	mov    %rdi,%rbp
  401c0d:	53                   	push   %rbx
  401c0e:	e8 ad 01 00 00       	call   401dc0 <strlen>
  401c13:	4c 89 e1             	mov    %r12,%rcx
  401c16:	48 89 ef             	mov    %rbp,%rdi
  401c19:	be 01 00 00 00       	mov    $0x1,%esi
  401c1e:	48 89 c2             	mov    %rax,%rdx
  401c21:	48 89 c3             	mov    %rax,%rbx
  401c24:	e8 f7 00 00 00       	call   401d20 <fwrite>
  401c29:	48 39 d8             	cmp    %rbx,%rax
  401c2c:	5b                   	pop    %rbx
  401c2d:	5d                   	pop    %rbp
  401c2e:	0f 95 c0             	setne  %al
  401c31:	41 5c                	pop    %r12
  401c33:	0f b6 c0             	movzbl %al,%eax
  401c36:	f7 d8                	neg    %eax
  401c38:	c3                   	ret    
  401c39:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

0000000000401c40 <__fwritex>:
  401c40:	f3 0f 1e fa          	endbr64 
  401c44:	41 56                	push   %r14
  401c46:	41 55                	push   %r13
  401c48:	49 89 f5             	mov    %rsi,%r13
  401c4b:	41 54                	push   %r12
  401c4d:	49 89 d4             	mov    %rdx,%r12
  401c50:	55                   	push   %rbp
  401c51:	48 89 fd             	mov    %rdi,%rbp
  401c54:	53                   	push   %rbx
  401c55:	48 8b 42 20          	mov    0x20(%rdx),%rax
  401c59:	48 85 c0             	test   %rax,%rax
  401c5c:	0f 84 96 00 00 00    	je     401cf8 <__fwritex+0xb8>
  401c62:	49 8b 7c 24 28       	mov    0x28(%r12),%rdi
  401c67:	48 29 f8             	sub    %rdi,%rax
  401c6a:	4c 39 e8             	cmp    %r13,%rax
  401c6d:	72 49                	jb     401cb8 <__fwritex+0x78>
  401c6f:	41 8b 84 24 90 00 00 	mov    0x90(%r12),%eax
  401c76:	00 
  401c77:	4c 89 eb             	mov    %r13,%rbx
  401c7a:	85 c0                	test   %eax,%eax
  401c7c:	79 30                	jns    401cae <__fwritex+0x6e>
  401c7e:	4d 89 ee             	mov    %r13,%r14
  401c81:	4c 89 f2             	mov    %r14,%rdx
  401c84:	48 89 ee             	mov    %rbp,%rsi
  401c87:	e8 b1 01 00 00       	call   401e3d <memcpy>
  401c8c:	4d 01 74 24 28       	add    %r14,0x28(%r12)
  401c91:	4c 89 e8             	mov    %r13,%rax
  401c94:	5b                   	pop    %rbx
  401c95:	5d                   	pop    %rbp
  401c96:	41 5c                	pop    %r12
  401c98:	41 5d                	pop    %r13
  401c9a:	41 5e                	pop    %r14
  401c9c:	c3                   	ret    
  401c9d:	0f 1f 00             	nopl   (%rax)
  401ca0:	80 7c 1d ff 0a       	cmpb   $0xa,-0x1(%rbp,%rbx,1)
  401ca5:	48 8d 43 ff          	lea    -0x1(%rbx),%rax
  401ca9:	74 25                	je     401cd0 <__fwritex+0x90>
  401cab:	48 89 c3             	mov    %rax,%rbx
  401cae:	48 85 db             	test   %rbx,%rbx
  401cb1:	75 ed                	jne    401ca0 <__fwritex+0x60>
  401cb3:	eb c9                	jmp    401c7e <__fwritex+0x3e>
  401cb5:	0f 1f 00             	nopl   (%rax)
  401cb8:	5b                   	pop    %rbx
  401cb9:	49 8b 44 24 48       	mov    0x48(%r12),%rax
  401cbe:	4c 89 ea             	mov    %r13,%rdx
  401cc1:	48 89 ee             	mov    %rbp,%rsi
  401cc4:	4c 89 e7             	mov    %r12,%rdi
  401cc7:	5d                   	pop    %rbp
  401cc8:	41 5c                	pop    %r12
  401cca:	41 5d                	pop    %r13
  401ccc:	41 5e                	pop    %r14
  401cce:	ff e0                	jmp    *%rax
  401cd0:	48 89 da             	mov    %rbx,%rdx
  401cd3:	48 89 ee             	mov    %rbp,%rsi
  401cd6:	4c 89 e7             	mov    %r12,%rdi
  401cd9:	41 ff 54 24 48       	call   *0x48(%r12)
  401cde:	48 39 c3             	cmp    %rax,%rbx
  401ce1:	77 b1                	ja     401c94 <__fwritex+0x54>
  401ce3:	4d 89 ee             	mov    %r13,%r14
  401ce6:	49 8b 7c 24 28       	mov    0x28(%r12),%rdi
  401ceb:	48 01 dd             	add    %rbx,%rbp
  401cee:	49 29 de             	sub    %rbx,%r14
  401cf1:	eb 8e                	jmp    401c81 <__fwritex+0x41>
  401cf3:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
  401cf8:	48 89 d7             	mov    %rdx,%rdi
  401cfb:	e8 90 fe ff ff       	call   401b90 <__towrite>
  401d00:	85 c0                	test   %eax,%eax
  401d02:	75 0c                	jne    401d10 <__fwritex+0xd0>
  401d04:	49 8b 44 24 20       	mov    0x20(%r12),%rax
  401d09:	e9 54 ff ff ff       	jmp    401c62 <__fwritex+0x22>
  401d0e:	66 90                	xchg   %ax,%ax
  401d10:	31 c0                	xor    %eax,%eax
  401d12:	eb 80                	jmp    401c94 <__fwritex+0x54>
  401d14:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  401d1b:	00 00 00 
  401d1e:	66 90                	xchg   %ax,%ax

0000000000401d20 <fwrite>:
  401d20:	f3 0f 1e fa          	endbr64 
  401d24:	41 57                	push   %r15
  401d26:	31 c0                	xor    %eax,%eax
  401d28:	41 56                	push   %r14
  401d2a:	49 89 f6             	mov    %rsi,%r14
  401d2d:	41 55                	push   %r13
  401d2f:	4c 0f af f2          	imul   %rdx,%r14
  401d33:	49 89 fd             	mov    %rdi,%r13
  401d36:	41 54                	push   %r12
  401d38:	49 89 cc             	mov    %rcx,%r12
  401d3b:	55                   	push   %rbp
  401d3c:	48 89 f5             	mov    %rsi,%rbp
  401d3f:	53                   	push   %rbx
  401d40:	48 89 d3             	mov    %rdx,%rbx
  401d43:	48 83 ec 08          	sub    $0x8,%rsp
  401d47:	48 85 f6             	test   %rsi,%rsi
  401d4a:	48 0f 44 d8          	cmove  %rax,%rbx
  401d4e:	8b 81 8c 00 00 00    	mov    0x8c(%rcx),%eax
  401d54:	85 c0                	test   %eax,%eax
  401d56:	79 30                	jns    401d88 <fwrite+0x68>
  401d58:	48 89 ca             	mov    %rcx,%rdx
  401d5b:	4c 89 f6             	mov    %r14,%rsi
  401d5e:	e8 dd fe ff ff       	call   401c40 <__fwritex>
  401d63:	49 89 c5             	mov    %rax,%r13
  401d66:	48 89 d8             	mov    %rbx,%rax
  401d69:	4d 39 ee             	cmp    %r13,%r14
  401d6c:	74 08                	je     401d76 <fwrite+0x56>
  401d6e:	4c 89 e8             	mov    %r13,%rax
  401d71:	31 d2                	xor    %edx,%edx
  401d73:	48 f7 f5             	div    %rbp
  401d76:	48 83 c4 08          	add    $0x8,%rsp
  401d7a:	5b                   	pop    %rbx
  401d7b:	5d                   	pop    %rbp
  401d7c:	41 5c                	pop    %r12
  401d7e:	41 5d                	pop    %r13
  401d80:	41 5e                	pop    %r14
  401d82:	41 5f                	pop    %r15
  401d84:	c3                   	ret    
  401d85:	0f 1f 00             	nopl   (%rax)
  401d88:	48 89 cf             	mov    %rcx,%rdi
  401d8b:	e8 40 fb ff ff       	call   4018d0 <__lockfile>
  401d90:	4c 89 ef             	mov    %r13,%rdi
  401d93:	4c 89 e2             	mov    %r12,%rdx
  401d96:	4c 89 f6             	mov    %r14,%rsi
  401d99:	41 89 c7             	mov    %eax,%r15d
  401d9c:	e8 9f fe ff ff       	call   401c40 <__fwritex>
  401da1:	49 89 c5             	mov    %rax,%r13
  401da4:	45 85 ff             	test   %r15d,%r15d
  401da7:	74 bd                	je     401d66 <fwrite+0x46>
  401da9:	4c 89 e7             	mov    %r12,%rdi
  401dac:	e8 ff fb ff ff       	call   4019b0 <__unlockfile>
  401db1:	eb b3                	jmp    401d66 <fwrite+0x46>
  401db3:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  401dba:	00 00 00 
  401dbd:	0f 1f 00             	nopl   (%rax)

0000000000401dc0 <strlen>:
  401dc0:	f3 0f 1e fa          	endbr64 
  401dc4:	48 89 f8             	mov    %rdi,%rax
  401dc7:	40 f6 c7 07          	test   $0x7,%dil
  401dcb:	75 0b                	jne    401dd8 <strlen+0x18>
  401dcd:	eb 19                	jmp    401de8 <strlen+0x28>
  401dcf:	90                   	nop
  401dd0:	48 83 c0 01          	add    $0x1,%rax
  401dd4:	a8 07                	test   $0x7,%al
  401dd6:	74 10                	je     401de8 <strlen+0x28>
  401dd8:	80 38 00             	cmpb   $0x0,(%rax)
  401ddb:	75 f3                	jne    401dd0 <strlen+0x10>
  401ddd:	48 29 f8             	sub    %rdi,%rax
  401de0:	c3                   	ret    
  401de1:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  401de8:	49 b8 ff fe fe fe fe 	movabs $0xfefefefefefefeff,%r8
  401def:	fe fe fe 
  401df2:	48 8b 10             	mov    (%rax),%rdx
  401df5:	48 be 80 80 80 80 80 	movabs $0x8080808080808080,%rsi
  401dfc:	80 80 80 
  401dff:	4a 8d 0c 02          	lea    (%rdx,%r8,1),%rcx
  401e03:	48 f7 d2             	not    %rdx
  401e06:	48 21 ca             	and    %rcx,%rdx
  401e09:	48 85 f2             	test   %rsi,%rdx
  401e0c:	75 26                	jne    401e34 <strlen+0x74>
  401e0e:	66 90                	xchg   %ax,%ax
  401e10:	48 8b 50 08          	mov    0x8(%rax),%rdx
  401e14:	48 83 c0 08          	add    $0x8,%rax
  401e18:	4a 8d 0c 02          	lea    (%rdx,%r8,1),%rcx
  401e1c:	48 f7 d2             	not    %rdx
  401e1f:	48 21 ca             	and    %rcx,%rdx
  401e22:	48 85 f2             	test   %rsi,%rdx
  401e25:	74 e9                	je     401e10 <strlen+0x50>
  401e27:	eb 0b                	jmp    401e34 <strlen+0x74>
  401e29:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  401e30:	48 83 c0 01          	add    $0x1,%rax
  401e34:	80 38 00             	cmpb   $0x0,(%rax)
  401e37:	75 f7                	jne    401e30 <strlen+0x70>
  401e39:	48 29 f8             	sub    %rdi,%rax
  401e3c:	c3                   	ret    

0000000000401e3d <memcpy>:
  401e3d:	48 89 f8             	mov    %rdi,%rax
  401e40:	48 83 fa 08          	cmp    $0x8,%rdx
  401e44:	72 14                	jb     401e5a <memcpy+0x1d>
  401e46:	f7 c7 07 00 00 00    	test   $0x7,%edi
  401e4c:	74 0c                	je     401e5a <memcpy+0x1d>
  401e4e:	a4                   	movsb  %ds:(%rsi),%es:(%rdi)
  401e4f:	48 ff ca             	dec    %rdx
  401e52:	f7 c7 07 00 00 00    	test   $0x7,%edi
  401e58:	75 f4                	jne    401e4e <memcpy+0x11>
  401e5a:	48 89 d1             	mov    %rdx,%rcx
  401e5d:	48 c1 e9 03          	shr    $0x3,%rcx
  401e61:	f3 48 a5             	rep movsq %ds:(%rsi),%es:(%rdi)
  401e64:	83 e2 07             	and    $0x7,%edx
  401e67:	74 05                	je     401e6e <memcpy+0x31>
  401e69:	a4                   	movsb  %ds:(%rsi),%es:(%rdi)
  401e6a:	ff ca                	dec    %edx
  401e6c:	75 fb                	jne    401e69 <memcpy+0x2c>
  401e6e:	c3                   	ret    

0000000000401e6f <__set_thread_area>:
  401e6f:	48 89 fe             	mov    %rdi,%rsi
  401e72:	bf 02 10 00 00       	mov    $0x1002,%edi
  401e77:	b8 9e 00 00 00       	mov    $0x9e,%eax
  401e7c:	0f 05                	syscall 
  401e7e:	c3                   	ret    
  401e7f:	90                   	nop

0000000000401e80 <__lseek>:
  401e80:	f3 0f 1e fa          	endbr64 
  401e84:	48 63 ff             	movslq %edi,%rdi
  401e87:	48 63 d2             	movslq %edx,%rdx
  401e8a:	b8 08 00 00 00       	mov    $0x8,%eax
  401e8f:	0f 05                	syscall 
  401e91:	48 89 c7             	mov    %rax,%rdi
  401e94:	e9 67 00 00 00       	jmp    401f00 <__syscall_ret>
  401e99:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

0000000000401ea0 <__init_ssp>:
  401ea0:	f3 0f 1e fa          	endbr64 
  401ea4:	53                   	push   %rbx
  401ea5:	48 85 ff             	test   %rdi,%rdi
  401ea8:	74 2e                	je     401ed8 <__init_ssp+0x38>
  401eaa:	48 c7 c3 b8 57 40 00 	mov    $0x4057b8,%rbx
  401eb1:	48 89 fe             	mov    %rdi,%rsi
  401eb4:	ba 08 00 00 00       	mov    $0x8,%edx
  401eb9:	48 89 df             	mov    %rbx,%rdi
  401ebc:	e8 7c ff ff ff       	call   401e3d <memcpy>
  401ec1:	48 8b 03             	mov    (%rbx),%rax
  401ec4:	64 48 8b 14 25 00 00 	mov    %fs:0x0,%rdx
  401ecb:	00 00 
  401ecd:	48 89 42 28          	mov    %rax,0x28(%rdx)
  401ed1:	5b                   	pop    %rbx
  401ed2:	c3                   	ret    
  401ed3:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
  401ed8:	48 c7 c2 b8 57 40 00 	mov    $0x4057b8,%rdx
  401edf:	48 69 c2 6d 4e c6 41 	imul   $0x41c64e6d,%rdx,%rax
  401ee6:	48 89 02             	mov    %rax,(%rdx)
  401ee9:	eb d9                	jmp    401ec4 <__init_ssp+0x24>
  401eeb:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)

0000000000401ef0 <__stack_chk_fail>:
  401ef0:	f3 0f 1e fa          	endbr64 
  401ef4:	f4                   	hlt    
  401ef5:	c3                   	ret    
  401ef6:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  401efd:	00 00 00 

0000000000401f00 <__syscall_ret>:
  401f00:	f3 0f 1e fa          	endbr64 
  401f04:	48 81 ff 00 f0 ff ff 	cmp    $0xfffffffffffff000,%rdi
  401f0b:	77 0b                	ja     401f18 <__syscall_ret+0x18>
  401f0d:	48 89 f8             	mov    %rdi,%rax
  401f10:	c3                   	ret    
  401f11:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  401f18:	48 83 ec 18          	sub    $0x18,%rsp
  401f1c:	48 89 7c 24 08       	mov    %rdi,0x8(%rsp)
  401f21:	e8 7a 03 00 00       	call   4022a0 <__errno_location>
  401f26:	48 8b 7c 24 08       	mov    0x8(%rsp),%rdi
  401f2b:	f7 df                	neg    %edi
  401f2d:	89 38                	mov    %edi,(%rax)
  401f2f:	48 c7 c0 ff ff ff ff 	mov    $0xffffffffffffffff,%rax
  401f36:	48 83 c4 18          	add    $0x18,%rsp
  401f3a:	c3                   	ret    
  401f3b:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)

0000000000401f40 <close_file>:
  401f40:	48 85 ff             	test   %rdi,%rdi
  401f43:	74 63                	je     401fa8 <close_file+0x68>
  401f45:	55                   	push   %rbp
  401f46:	8b 87 8c 00 00 00    	mov    0x8c(%rdi),%eax
  401f4c:	48 89 fd             	mov    %rdi,%rbp
  401f4f:	85 c0                	test   %eax,%eax
  401f51:	79 3d                	jns    401f90 <close_file+0x50>
  401f53:	48 8b 45 38          	mov    0x38(%rbp),%rax
  401f57:	48 39 45 28          	cmp    %rax,0x28(%rbp)
  401f5b:	74 0a                	je     401f67 <close_file+0x27>
  401f5d:	31 d2                	xor    %edx,%edx
  401f5f:	31 f6                	xor    %esi,%esi
  401f61:	48 89 ef             	mov    %rbp,%rdi
  401f64:	ff 55 48             	call   *0x48(%rbp)
  401f67:	48 8b 75 08          	mov    0x8(%rbp),%rsi
  401f6b:	48 8b 45 10          	mov    0x10(%rbp),%rax
  401f6f:	48 39 c6             	cmp    %rax,%rsi
  401f72:	74 2c                	je     401fa0 <close_file+0x60>
  401f74:	48 29 c6             	sub    %rax,%rsi
  401f77:	48 89 ef             	mov    %rbp,%rdi
  401f7a:	48 8b 45 50          	mov    0x50(%rbp),%rax
  401f7e:	ba 01 00 00 00       	mov    $0x1,%edx
  401f83:	5d                   	pop    %rbp
  401f84:	ff e0                	jmp    *%rax
  401f86:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  401f8d:	00 00 00 
  401f90:	e8 3b f9 ff ff       	call   4018d0 <__lockfile>
  401f95:	eb bc                	jmp    401f53 <close_file+0x13>
  401f97:	66 0f 1f 84 00 00 00 	nopw   0x0(%rax,%rax,1)
  401f9e:	00 00 
  401fa0:	5d                   	pop    %rbp
  401fa1:	c3                   	ret    
  401fa2:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  401fa8:	c3                   	ret    
  401fa9:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)

0000000000401fb0 <__stdio_exit>:
  401fb0:	f3 0f 1e fa          	endbr64 
  401fb4:	53                   	push   %rbx
  401fb5:	e8 76 01 00 00       	call   402130 <__ofl_lock>
  401fba:	48 8b 18             	mov    (%rax),%rbx
  401fbd:	48 85 db             	test   %rbx,%rbx
  401fc0:	74 17                	je     401fd9 <__stdio_exit+0x29>
  401fc2:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
  401fc8:	48 89 df             	mov    %rbx,%rdi
  401fcb:	e8 70 ff ff ff       	call   401f40 <close_file>
  401fd0:	48 8b 5b 70          	mov    0x70(%rbx),%rbx
  401fd4:	48 85 db             	test   %rbx,%rbx
  401fd7:	75 ef                	jne    401fc8 <__stdio_exit+0x18>
  401fd9:	48 8b 3d e0 37 00 00 	mov    0x37e0(%rip),%rdi        # 4057c0 <__stderr_used>
  401fe0:	e8 5b ff ff ff       	call   401f40 <close_file>
  401fe5:	48 8b 3d 3c 30 00 00 	mov    0x303c(%rip),%rdi        # 405028 <__stdout_used>
  401fec:	e8 4f ff ff ff       	call   401f40 <close_file>
  401ff1:	48 8b 3d c8 37 00 00 	mov    0x37c8(%rip),%rdi        # 4057c0 <__stderr_used>
  401ff8:	5b                   	pop    %rbx
  401ff9:	e9 42 ff ff ff       	jmp    401f40 <close_file>
  401ffe:	66 90                	xchg   %ax,%ax

0000000000402000 <__stdio_write>:
  402000:	f3 0f 1e fa          	endbr64 
  402004:	41 57                	push   %r15
  402006:	49 89 ff             	mov    %rdi,%r15
  402009:	41 56                	push   %r14
  40200b:	41 55                	push   %r13
  40200d:	41 54                	push   %r12
  40200f:	41 bc 14 00 00 00    	mov    $0x14,%r12d
  402015:	55                   	push   %rbp
  402016:	bd 02 00 00 00       	mov    $0x2,%ebp
  40201b:	53                   	push   %rbx
  40201c:	bb 02 00 00 00       	mov    $0x2,%ebx
  402021:	48 83 ec 48          	sub    $0x48,%rsp
  402025:	4c 8b 77 28          	mov    0x28(%rdi),%r14
  402029:	48 89 54 24 08       	mov    %rdx,0x8(%rsp)
  40202e:	4c 8d 6c 24 10       	lea    0x10(%rsp),%r13
  402033:	64 48 8b 04 25 28 00 	mov    %fs:0x28,%rax
  40203a:	00 00 
  40203c:	48 89 44 24 38       	mov    %rax,0x38(%rsp)
  402041:	48 8b 47 38          	mov    0x38(%rdi),%rax
  402045:	48 89 74 24 20       	mov    %rsi,0x20(%rsp)
  40204a:	48 89 54 24 28       	mov    %rdx,0x28(%rsp)
  40204f:	49 29 c6             	sub    %rax,%r14
  402052:	48 89 44 24 10       	mov    %rax,0x10(%rsp)
  402057:	4c 89 74 24 18       	mov    %r14,0x18(%rsp)
  40205c:	49 01 d6             	add    %rdx,%r14
  40205f:	eb 38                	jmp    402099 <__stdio_write+0x99>
  402061:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  402068:	48 85 c0             	test   %rax,%rax
  40206b:	0f 88 7f 00 00 00    	js     4020f0 <__stdio_write+0xf0>
  402071:	49 8b 55 08          	mov    0x8(%r13),%rdx
  402075:	49 29 c6             	sub    %rax,%r14
  402078:	48 39 d0             	cmp    %rdx,%rax
  40207b:	76 11                	jbe    40208e <__stdio_write+0x8e>
  40207d:	83 ed 01             	sub    $0x1,%ebp
  402080:	48 29 d0             	sub    %rdx,%rax
  402083:	49 83 c5 10          	add    $0x10,%r13
  402087:	49 8b 55 08          	mov    0x8(%r13),%rdx
  40208b:	48 63 dd             	movslq %ebp,%rbx
  40208e:	48 29 c2             	sub    %rax,%rdx
  402091:	49 01 45 00          	add    %rax,0x0(%r13)
  402095:	49 89 55 08          	mov    %rdx,0x8(%r13)
  402099:	49 63 7f 78          	movslq 0x78(%r15),%rdi
  40209d:	4c 89 e0             	mov    %r12,%rax
  4020a0:	4c 89 ee             	mov    %r13,%rsi
  4020a3:	48 89 da             	mov    %rbx,%rdx
  4020a6:	0f 05                	syscall 
  4020a8:	48 89 c7             	mov    %rax,%rdi
  4020ab:	e8 50 fe ff ff       	call   401f00 <__syscall_ret>
  4020b0:	49 39 c6             	cmp    %rax,%r14
  4020b3:	75 b3                	jne    402068 <__stdio_write+0x68>
  4020b5:	49 8b 47 58          	mov    0x58(%r15),%rax
  4020b9:	49 8b 57 60          	mov    0x60(%r15),%rdx
  4020bd:	48 01 c2             	add    %rax,%rdx
  4020c0:	49 89 47 38          	mov    %rax,0x38(%r15)
  4020c4:	49 89 57 20          	mov    %rdx,0x20(%r15)
  4020c8:	49 89 47 28          	mov    %rax,0x28(%r15)
  4020cc:	48 8b 44 24 08       	mov    0x8(%rsp),%rax
  4020d1:	48 8b 54 24 38       	mov    0x38(%rsp),%rdx
  4020d6:	64 48 2b 14 25 28 00 	sub    %fs:0x28,%rdx
  4020dd:	00 00 
  4020df:	75 3d                	jne    40211e <__stdio_write+0x11e>
  4020e1:	48 83 c4 48          	add    $0x48,%rsp
  4020e5:	5b                   	pop    %rbx
  4020e6:	5d                   	pop    %rbp
  4020e7:	41 5c                	pop    %r12
  4020e9:	41 5d                	pop    %r13
  4020eb:	41 5e                	pop    %r14
  4020ed:	41 5f                	pop    %r15
  4020ef:	c3                   	ret    
  4020f0:	41 83 0f 20          	orl    $0x20,(%r15)
  4020f4:	31 c0                	xor    %eax,%eax
  4020f6:	49 c7 47 20 00 00 00 	movq   $0x0,0x20(%r15)
  4020fd:	00 
  4020fe:	49 c7 47 38 00 00 00 	movq   $0x0,0x38(%r15)
  402105:	00 
  402106:	49 c7 47 28 00 00 00 	movq   $0x0,0x28(%r15)
  40210d:	00 
  40210e:	83 fd 02             	cmp    $0x2,%ebp
  402111:	74 be                	je     4020d1 <__stdio_write+0xd1>
  402113:	48 8b 44 24 08       	mov    0x8(%rsp),%rax
  402118:	49 2b 45 08          	sub    0x8(%r13),%rax
  40211c:	eb b3                	jmp    4020d1 <__stdio_write+0xd1>
  40211e:	e8 cd fd ff ff       	call   401ef0 <__stack_chk_fail>
  402123:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  40212a:	00 00 00 
  40212d:	0f 1f 00             	nopl   (%rax)

0000000000402130 <__ofl_lock>:
  402130:	f3 0f 1e fa          	endbr64 
  402134:	48 83 ec 08          	sub    $0x8,%rsp
  402138:	48 8d 3d 89 36 00 00 	lea    0x3689(%rip),%rdi        # 4057c8 <ofl_lock>
  40213f:	e8 1c 00 00 00       	call   402160 <__lock>
  402144:	48 8d 05 85 36 00 00 	lea    0x3685(%rip),%rax        # 4057d0 <ofl_head>
  40214b:	48 83 c4 08          	add    $0x8,%rsp
  40214f:	c3                   	ret    

0000000000402150 <__ofl_unlock>:
  402150:	f3 0f 1e fa          	endbr64 
  402154:	48 8d 3d 6d 36 00 00 	lea    0x366d(%rip),%rdi        # 4057c8 <ofl_lock>
  40215b:	e9 f0 00 00 00       	jmp    402250 <__unlock>

0000000000402160 <__lock>:
  402160:	f3 0f 1e fa          	endbr64 
  402164:	0f be 0d 18 30 00 00 	movsbl 0x3018(%rip),%ecx        # 405183 <__libc+0x3>
  40216b:	85 c9                	test   %ecx,%ecx
  40216d:	74 51                	je     4021c0 <__lock+0x60>
  40216f:	31 c0                	xor    %eax,%eax
  402171:	ba 01 00 00 80       	mov    $0x80000001,%edx
  402176:	f0 0f b1 17          	lock cmpxchg %edx,(%rdi)
  40217a:	89 c2                	mov    %eax,%edx
  40217c:	85 c9                	test   %ecx,%ecx
  40217e:	78 48                	js     4021c8 <__lock+0x68>
  402180:	85 c0                	test   %eax,%eax
  402182:	74 3c                	je     4021c0 <__lock+0x60>
  402184:	41 b8 0a 00 00 00    	mov    $0xa,%r8d
  40218a:	eb 1e                	jmp    4021aa <__lock+0x4a>
  40218c:	0f 1f 40 00          	nopl   0x0(%rax)
  402190:	8d b2 01 00 00 80    	lea    -0x7fffffff(%rdx),%esi
  402196:	89 d1                	mov    %edx,%ecx
  402198:	89 c8                	mov    %ecx,%eax
  40219a:	f0 0f b1 37          	lock cmpxchg %esi,(%rdi)
  40219e:	89 c2                	mov    %eax,%edx
  4021a0:	39 c1                	cmp    %eax,%ecx
  4021a2:	74 1c                	je     4021c0 <__lock+0x60>
  4021a4:	41 83 e8 01          	sub    $0x1,%r8d
  4021a8:	74 2e                	je     4021d8 <__lock+0x78>
  4021aa:	85 d2                	test   %edx,%edx
  4021ac:	79 e2                	jns    402190 <__lock+0x30>
  4021ae:	8d 8a ff ff ff 7f    	lea    0x7fffffff(%rdx),%ecx
  4021b4:	89 d6                	mov    %edx,%esi
  4021b6:	eb e0                	jmp    402198 <__lock+0x38>
  4021b8:	0f 1f 84 00 00 00 00 	nopl   0x0(%rax,%rax,1)
  4021bf:	00 
  4021c0:	c3                   	ret    
  4021c1:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  4021c8:	c6 05 b4 2f 00 00 00 	movb   $0x0,0x2fb4(%rip)        # 405183 <__libc+0x3>
  4021cf:	85 c0                	test   %eax,%eax
  4021d1:	75 b1                	jne    402184 <__lock+0x24>
  4021d3:	eb eb                	jmp    4021c0 <__lock+0x60>
  4021d5:	0f 1f 00             	nopl   (%rax)
  4021d8:	53                   	push   %rbx
  4021d9:	41 b8 01 00 00 00    	mov    $0x1,%r8d
  4021df:	f0 44 0f c1 07       	lock xadd %r8d,(%rdi)
  4021e4:	41 b9 ca 00 00 00    	mov    $0xca,%r9d
  4021ea:	41 83 c0 01          	add    $0x1,%r8d
  4021ee:	bb 80 00 00 00       	mov    $0x80,%ebx
  4021f3:	eb 18                	jmp    40220d <__lock+0xad>
  4021f5:	0f 1f 00             	nopl   (%rax)
  4021f8:	44 8d 82 00 00 00 80 	lea    -0x80000000(%rdx),%r8d
  4021ff:	89 d0                	mov    %edx,%eax
  402201:	f0 44 0f b1 07       	lock cmpxchg %r8d,(%rdi)
  402206:	41 89 c0             	mov    %eax,%r8d
  402209:	39 c2                	cmp    %eax,%edx
  40220b:	74 33                	je     402240 <__lock+0xe0>
  40220d:	44 89 c2             	mov    %r8d,%edx
  402210:	45 85 c0             	test   %r8d,%r8d
  402213:	79 e3                	jns    4021f8 <__lock+0x98>
  402215:	49 63 d0             	movslq %r8d,%rdx
  402218:	45 31 d2             	xor    %r10d,%r10d
  40221b:	4c 89 c8             	mov    %r9,%rax
  40221e:	48 89 de             	mov    %rbx,%rsi
  402221:	0f 05                	syscall 
  402223:	48 83 f8 da          	cmp    $0xffffffffffffffda,%rax
  402227:	75 07                	jne    402230 <__lock+0xd0>
  402229:	31 f6                	xor    %esi,%esi
  40222b:	4c 89 c8             	mov    %r9,%rax
  40222e:	0f 05                	syscall 
  402230:	41 8d 90 ff ff ff 7f 	lea    0x7fffffff(%r8),%edx
  402237:	eb bf                	jmp    4021f8 <__lock+0x98>
  402239:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
  402240:	5b                   	pop    %rbx
  402241:	c3                   	ret    
  402242:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  402249:	00 00 00 
  40224c:	0f 1f 40 00          	nopl   0x0(%rax)

0000000000402250 <__unlock>:
  402250:	f3 0f 1e fa          	endbr64 
  402254:	8b 07                	mov    (%rdi),%eax
  402256:	85 c0                	test   %eax,%eax
  402258:	78 06                	js     402260 <__unlock+0x10>
  40225a:	c3                   	ret    
  40225b:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
  402260:	b8 ff ff ff 7f       	mov    $0x7fffffff,%eax
  402265:	f0 0f c1 07          	lock xadd %eax,(%rdi)
  402269:	3d 01 00 00 80       	cmp    $0x80000001,%eax
  40226e:	74 ea                	je     40225a <__unlock+0xa>
  402270:	41 b8 ca 00 00 00    	mov    $0xca,%r8d
  402276:	ba 01 00 00 00       	mov    $0x1,%edx
  40227b:	be 81 00 00 00       	mov    $0x81,%esi
  402280:	4c 89 c0             	mov    %r8,%rax
  402283:	0f 05                	syscall 
  402285:	48 83 f8 da          	cmp    $0xffffffffffffffda,%rax
  402289:	75 cf                	jne    40225a <__unlock+0xa>
  40228b:	4c 89 c0             	mov    %r8,%rax
  40228e:	48 89 d6             	mov    %rdx,%rsi
  402291:	0f 05                	syscall 
  402293:	c3                   	ret    
  402294:	66 2e 0f 1f 84 00 00 	cs nopw 0x0(%rax,%rax,1)
  40229b:	00 00 00 
  40229e:	66 90                	xchg   %ax,%ax

00000000004022a0 <__errno_location>:
  4022a0:	f3 0f 1e fa          	endbr64 
  4022a4:	64 48 8b 04 25 00 00 	mov    %fs:0x0,%rax
  4022ab:	00 00 
  4022ad:	48 83 c0 34          	add    $0x34,%rax
  4022b1:	c3                   	ret    

Disassembly of section .fini:

00000000004022b2 <_fini>:
  4022b2:	50                   	push   %rax
  4022b3:	58                   	pop    %rax
  4022b4:	c3                   	ret    
