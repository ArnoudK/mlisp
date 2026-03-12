define ptr addrspace(1) @test(ptr addrspace(1) %obj) gc "statepoint-example" {
entry:
  %safepoint = call token (i64, i32, ptr, i32, i32, ...) @llvm.experimental.gc.statepoint.p0f_isVoidf(i64 0, i32 0, ptr elementtype(void ()) @foo, i32 0, i32 0, i32 0, i32 0) ["gc-live"(ptr addrspace(1) %obj)]
  %obj.relocated = call ptr addrspace(1) @llvm.experimental.gc.relocate.p1(token %safepoint, i32 0, i32 0)
  ret ptr addrspace(1) %obj.relocated
}

define void @foo() {
entry:
  ret void
}

declare token @llvm.experimental.gc.statepoint.p0f_isVoidf(i64 immarg, i32 immarg, ptr elementtype(void ()), i32 immarg, i32 immarg, ...)
declare ptr addrspace(1) @llvm.experimental.gc.relocate.p1(token, i32 immarg, i32 immarg)
