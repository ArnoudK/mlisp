define void @gc.safepoint_poll() {
entry:
  call void @gc_safepoint_poll()
  ret void
}

declare void @gc_safepoint_poll()

define void @foo(ptr addrspace(1) %obj) {
entry:
  ret void
}

define ptr addrspace(1) @test(ptr addrspace(1) %obj) gc "coreclr" {
entry:
  call void @foo(ptr addrspace(1) %obj)
  ret ptr addrspace(1) %obj
}
