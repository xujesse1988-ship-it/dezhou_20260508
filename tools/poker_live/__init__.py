"""牌桌 driver 共享件（openpoker_play.py / wopo_play.py 共用）。

分层：通讯（各服务自己的 transport）→ 牌桌逻辑校验（table.HandEngine）→ 打牌决策
（decision + advisor_client，策略全在常驻 Rust advisor）。
"""
