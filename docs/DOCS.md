# Kalatori docs

## Milestone 1 state

### Decisions
1. Amounts передаем флотами, демон конфвертит у себя в u128 

### Daemon API

// давайте подумаем какого хуя все эти параметры в урле, может есть смысл пересмотреть

- `[METHOD(GET/POST/PUT...)] /order/123/price/30`
// provide resonse 
- `[METHOD(GET/POST/PUT...)] /recipient/0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d/order/123/price/30`
// provide resonse 

### Statuses
// еще статусы
- order
- - waiting
- - paid

// балансы/валюты
