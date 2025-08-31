# DXLink Examples

This directory contains examples demonstrating how to use the DXLink library with tastytrade's sandbox environment.

## Setup

### 1. Environment Configuration

Copy the `.env.example` file to `.env` in the project root:

```bash
cp .env.example .env
```

Edit the `.env` file and configure your settings:

```bash
# Required: Your tastytrade API token
DXLINK_API_TOKEN=your_actual_token_here

# Optional: Custom WebSocket URL (defaults to demo server)
DXLINK_WS_URL=wss://demo.dxfeed.com/dxlink-ws

# Optional: Enable debug logging
RUST_LOG=debug
```

### 2. Getting Your API Token

To get your tastytrade API token:

1. Log into your tastytrade account
2. Navigate to API settings
3. Generate or copy your API token
4. Add it to your `.env` file

**Note:** For testing purposes, you can run the examples without a token using the demo server.

## Running Examples

### Basic Example

The basic example demonstrates connecting to DXLink, subscribing to market data, and processing events:

```bash
# Run with environment variables from .env file
cargo run --bin basic

# Or run with explicit environment variables
DXLINK_API_TOKEN=your_token RUST_LOG=info cargo run --bin basic
```

### Example Features

- **Connection Management**: Establishes WebSocket connection to DXLink server
- **Authentication**: Uses API token for authentication (optional for demo)
- **Channel Creation**: Creates feed channels for market data
- **Event Subscription**: Subscribes to Quote and Trade events
- **Real-time Processing**: Processes incoming market events
- **Multiple Symbols**: Demonstrates subscribing to multiple symbols (AAPL, MSFT, BTC/USD)

## Environment Variables Reference

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `DXLINK_API_TOKEN` | tastytrade API authentication token | empty | No (for demo) |
| `DXLINK_WS_URL` | DXLink WebSocket server URL | `wss://demo.dxfeed.com/dxlink-ws` | No |
| `RUST_LOG` | Logging level (error, warn, info, debug, trace) | info | No |

## Troubleshooting

### Common Issues

1. **Authentication Errors**
   - Verify your API token is correct
   - Ensure the token has appropriate permissions
   - Check if you're using the correct server URL

2. **Connection Issues**
   - Verify network connectivity
   - Check if the WebSocket URL is accessible
   - Ensure firewall/proxy settings allow WebSocket connections

3. **No Data Received**
   - Verify your subscriptions are correct
   - Check if the symbols you're subscribing to are valid
   - Ensure the market is open (for live data)

### Debug Mode

Enable debug logging to see detailed information:

```bash
RUST_LOG=debug cargo run --bin basic
```

## Security Notes

- Never commit your `.env` file to version control
- Keep your API tokens secure and rotate them regularly
- Use the sandbox/demo environment for testing
- The `.env` file is already included in `.gitignore`

## Next Steps

- Explore the DXLink library documentation
- Check out the integration tests for more advanced usage
- Review the tastytrade API documentation for additional features