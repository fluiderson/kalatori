# Kalatori docs

## Milestone 1 state

### Decisions
1. Amounts передаем флотами, демон конфвертит у себя в u128 

# Daemon API

## Overview

API for interacting with the daemon. 

## Endpoints

### Health Check

- **Endpoint**: `GET /health`
- **Description**: Returns the status of the API and its current version.

- **Response (200 OK)**:
    ```json
    {
        "version": string
    }
    ```

### Create or get Account if exists

- **Endpoint**: `POST /account`
- **Description**: Creates a new account, returns account if exists already

- **Request**:
    ```json
    {
        "order_id": string, 
        "amount": float,
        "currency": {
            "type": string,
            "code": string
        }
    }
    ```

- **Response (201 Created/200 OK)**:
    ```json
    {
        "status": string:pending|paid,
        "address": string,
        "order_id": string,
        "rpc": string,
        "decimals": integer,
        "amount": float
    }
    ```

- **Response(400 Bad Request)**: 
    ```json
    {
        "error": string
    }
    ```
    
### Delete Account

- **Endpoint**: `DELETE /account/{order_id}`
- **Description**: Deletes account from daemon kv store

- **Response (200 OK)**:

## Common Responses for Errors

- **404 Not Found**:
    - Empty body

- **500 Internal Server Error**:
    ```json
    {
        "error": string
    }
    ```


