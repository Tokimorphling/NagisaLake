# 性能热点复核与本轮修复

范围：Hub 调度/内存状态、PostgreSQL job/artifact 路径、Worker journal/字节传输，以及新 Agent caller。
这是基于真实调用路径的静态分析与回归验证，**没有生产流量 profile，也没有声称获得某个吞吐提升百分比**。
此前子代理审查多数因限流/超时未完成；结论以主线程复核的代码及测试为准。

## 已处理

| 优先级 | 热点及原代价 | 本轮实现 |
| --- | --- | --- |
| P1 | `hub/state.rs` 缓存每次插入 `retain` + `min_by_key` 全扫描 | 泛型 `TtlCache<T>` 按插入/到期顺序淘汰，均摊常数时间；重复更新的队列元数据也限制在容量的两倍以内 |
| P1 | `QuotaGate::acquire` 每次扫所有组织，并反复创建空闲锁 | 按容量增长阈值清理；命中时借用 key；清理不替换仍持有/等待中的锁 |
| P1 | `ratelimit.rs` 全局互斥锁、每请求拼接 key、饱和时全扫 | 16 分片、借用键 HashTable 查找、有界 recency 队列；命中不触发容量淘汰，避免顺带重置已有调用者的额度 |
| P1 | Hub 启动对每个 job 过滤全部未结束事件，`O(J×E)` | 事件按组织/job 一次索引，整体 `O(J+E)`；仅构建最后 256 条缓存事件 |
| P1 | 精确设备已获授权的 scheduler 仍全局扫描 worker | `reserve_capacity_on` 单键查找并在同一临界区检查容量/预留 |
| P1 | `send_command` 深拷贝 WorkerSession 的 manifests/labels/预留集合 | 仅克隆 channel handles；队列发送的背压也纳入 ACK deadline |
| P1 | outbox 的 32 个 job 逐个等待 ACK，一个慢 worker 挡住其余任务 | claim 数量继续有界，但独立 job 的交付/ACK 并发执行 |
| P1 | `commit_new_job` 持租户 quota 锁时逐个 UPDATE 输入 artifact | 一次 `UPDATE ... id=ANY(...) RETURNING`；数量不匹配仍在原事务内整体失败 |
| P1 | outbox 对 N 个输入发出 N 次串行 SELECT | `artifacts_by_ids` 一次查回，用 map 按原输入顺序恢复；presign 是本地签名，不误算成远程 S3 请求 |
| P1 | journal 每个事件解码完整 `dispatch_json` | SQLite 事件路径只读取 state/sequence/pending event；与 MemoryJournal 共用验证逻辑 |
| P1 | WS/SMUX bridge 在 select 的一个分支里等待完整写入，双向互相阻塞 | 两个完整方向 future 同时轮询，共用泛型 bridge；ping/pong 队列有界，不额外逐消息 spawn |
| P1 | JSON line receive 每次读之后从头扫描，大片段下近似二次扫描 | 保存扫描偏移；复用读中转及序列化缓冲；继续检查 frame 大小和短写 |
| P2 | bridge 的 `BytesMut::split()` 消耗可用尾部后退化为小读取 | 每次读取前预留 32 KiB |
| P2 | 多 GiB 上传默认 4 KiB ReaderStream，下载按网络小块写盘 | 256 KiB 读块与 BufWriter；保留大小/hash、签名有效期、取消和重试约束；输出目录每 job 创建一次 |

### 未以性能为名削弱的保证

- SQLite 明确保留 `synchronous=FULL`。不能把“允许重放”误解成“丢失本地 durable event 也能恢复”。
- `SetPendingEvent` 与收到 Hub ACK 后的 `ClearPendingEvent` 不合并：网络确认位于二者之间。
- quota、artifact claim、job/outbox/idempotency 仍在原有 PostgreSQL 事务内。
- 输出 artifact 的确认顺序仍按原作业顺序，不直接并发化这条有可观察顺序的链。
- 保留认证、授权、frame/对象大小上限、背压和并发 admission。

## 回归测试关注点

- 热 key 反复刷新，缓存与 limiter 的辅助队列不会无界增长。
- limiter 满分片时，命中最旧 key 不会被意外淘汰后重新拿到额度。
- quota lock 经清理后，原持有者仍排斥同组织的第二次获取。
- worker outbound 队列满时在 deadline 内结束，未发出的 reservation 被释放。
- WebSocket send 永久 Pending 时，反方向 ACK 字节仍可到达 SMUX。
- 大 JSON/UTF-8 跨小分片、空行、EOF、短写、超限及缓冲复用。
- journal 的 dispatch JSON 故意不可解码时，合法事件更新仍成功；重复/冲突序列检查不变。
- 上传重试继续带完整 body；403 不重试；刷新即将到期的签名。

Windows 测试中还定位到 mock PUT handler 不消费 body 就返回状态会造成 TCP reset (`10053`)，
使原本测 403 的用例误入网络重试并等待不存在的下一张 ticket。测试服务器现在消费 body 后再返回，
没有通过修改生产错误分类或缩短大文件上传 timeout 来掩盖该问题。

## 代码组织

原有公共重导出保持不变：

| crate | 原 lib.rs | 新 lib.rs | 主要模块 |
| --- | ---: | ---: | --- |
| `nagisalake-runtime` | 2070 行 | 32 行 | bus、runner、artifacts、error、util、tests |
| `nagisalake-transport` | 1000 行 | 21 行 | codec、bridge、connect、hub、tls、error、tests |
| `nagisalake-journal` | 608 行 | 14 行 | sqlite、memory、state、error、tests |

新 `nagisalake-agent` 从一开始按契约、监督、HTTP、SSE 路由和持久化事件协调分模块，避免继续增加
一个上千行的 lib.rs。Hub 的配置/授权/组织限制和数据库记录仍留在相应业务层。

## 验证与剩余测量

```bash
cargo +nightly fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
(cd web && npm run typecheck && npm test && npm run build)
```

PostgreSQL 用例需要显式设置 `NAGISALAKE_TEST_DATABASE_URL`；未设置时，测试框架的绿色结果包含
跳过数据库操作的用例，不能宣称实测了 SQL 执行计划或事务并发性能。

后续负载测量应分别覆盖大 active-job 集合的恢复时间、满容量缓存写入、满 limiter 分片、慢 worker
对其他 worker 派发延迟，以及 4 KiB/256 KiB 文件传输的 CPU/syscall 数。使用已存在的 loadgen 和
Hub metrics 观测 p95/p99/池等待，避免只看平均耗时。scheduler 的 pass 数量/周期、数据库池尺寸、
历史事件读取和多输出传输并行度仍需要结合真实负载调优，不能凭静态阅读保证它们不再成为下一处瓶颈。
