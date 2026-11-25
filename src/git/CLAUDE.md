# src/git/
> L2 | 父级: ../CLAUDE.md

成员列表

mod.rs: 模块声明；导出 oid、object、delta、wire、advertise、pack、push、fetch。
oid.rs: Git 对象 ID 的唯一计算处（`<type> <len>\0` + 内容的 SHA-1）与对象头生成。
object.rs: 提交、树、标签的解析与 reachable 可达性遍历；跳过 gitlink 条目，缺失对象报错。
delta.rs: 应用 Git delta 指令，支持 copy/insert 与 65536 特例；长度不符即拒绝。
wire.rs: pkt-line 编解码与 flush 处理；传输层与协议核心共用的线格式。
advertise.rs: 生成引用通告（HEAD 固定指向 refs/heads/main、能力行、标签 peel）；空仓库使用 capabilities^{} 行。
pack.rs: 用 gix-pack 解码推送包（含薄包外部基对象与差量还原），并编码不含差量的拉取包。
push.rs: 推送语义的唯一实现；解析命令、入库对象、比较并交换更新引用、写 report-status。
fetch.rs: 拉取语义的唯一实现；忽略 have 协商，发送 wants 可达对象，先回 NAK 再回 pack。

[PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
