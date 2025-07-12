#![allow(unexpected_cfgs)]
#![allow(deprecated)]
use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer, Mint};
use anchor_spl::associated_token::AssociatedToken;

declare_id!("AuctionProgram111111111111111111111111111111");

#[program]
pub mod auction {
    use super::*;

    /// Create a new auction
    pub fn create_auction(
        ctx: Context<CreateAuction>,
        start_time: i64,
        end_time: i64,
        reserve_price: u64,
        min_bid_increment: u64,
    ) -> Result<()> {
        // Validate marketplace is active
        require!(!ctx.accounts.marketplace.is_paused, AuctionError::MarketplacePaused);
        
        let clock = Clock::get()?;
        
        // Validate auction timing
        require!(start_time >= clock.unix_timestamp, AuctionError::InvalidStartTime);
        require!(end_time > start_time, AuctionError::InvalidEndTime);
        require!(reserve_price > 0, AuctionError::InvalidReservePrice);
        require!(min_bid_increment > 0, AuctionError::InvalidBidIncrement);
        
        // Duration must be at least 1 hour and max 30 days
        let duration = end_time - start_time;
        require!(duration >= 3600, AuctionError::AuctionTooShort); // 1 hour
        require!(duration <= 2592000, AuctionError::AuctionTooLong); // 30 days

        // Transfer NFT to auction escrow
        let transfer_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.seller_token_account.to_account_info(),
                to: ctx.accounts.auction_token_account.to_account_info(),
                authority: ctx.accounts.seller.to_account_info(),
            },
        );
        token::transfer(transfer_ctx, 1)?;

        // Initialize auction state
        let auction = &mut ctx.accounts.auction;
        auction.seller = ctx.accounts.seller.key();
        auction.mint = ctx.accounts.mint.key();
        auction.start_time = start_time;
        auction.end_time = end_time;
        auction.reserve_price = reserve_price;
        auction.min_bid_increment = min_bid_increment;
        auction.highest_bid = 0;
        auction.highest_bidder = None;
        auction.total_bids = 0;
        auction.is_settled = false;
        auction.is_canceled = false;
        auction.bump = ctx.bumps.auction;

        emit!(AuctionCreated {
            auction: auction.key(),
            seller: auction.seller,
            mint: auction.mint,
            start_time,
            end_time,
            reserve_price,
            min_bid_increment,
        });

        Ok(())
    }

    /// Place a bid on an auction
    pub fn place_bid(ctx: Context<PlaceBid>, bid_amount: u64) -> Result<()> {
        let clock = Clock::get()?;
        
        // Validate auction state (read-only access)
        require!(!ctx.accounts.auction.is_settled, AuctionError::AuctionAlreadySettled);
        require!(!ctx.accounts.auction.is_canceled, AuctionError::AuctionCanceled);
        require!(clock.unix_timestamp >= ctx.accounts.auction.start_time, AuctionError::AuctionNotStarted);
        require!(clock.unix_timestamp < ctx.accounts.auction.end_time, AuctionError::AuctionEnded);
        require!(bid_amount >= ctx.accounts.auction.reserve_price, AuctionError::BidBelowReserve);
        
        // Validate bid amount
        let required_bid = if ctx.accounts.auction.highest_bid == 0 {
            ctx.accounts.auction.reserve_price
        } else {
            ctx.accounts.auction.highest_bid.checked_add(ctx.accounts.auction.min_bid_increment)
                .ok_or(AuctionError::MathOverflow)?
        };
        require!(bid_amount >= required_bid, AuctionError::BidTooLow);

        // Store values before mutable access
        let previous_bidder = ctx.accounts.auction.highest_bidder;
        let previous_bid = ctx.accounts.auction.highest_bid;
        let auction_bump = ctx.accounts.auction.bump;
        let mint_key = ctx.accounts.auction.mint;
        let seller_key = ctx.accounts.auction.seller;

        // Refund previous highest bidder
        if let Some(previous_bidder_key) = previous_bidder {
            if previous_bid > 0 {
                // Transfer previous bid back to previous bidder
                let auction_seeds = &[
                    b"auction",
                    mint_key.as_ref(),
                    seller_key.as_ref(),
                    &[auction_bump],
                ];
                let signer = &[&auction_seeds[..]];

                let refund_ctx = CpiContext::new_with_signer(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.auction.to_account_info(),
                        to: ctx.accounts.previous_bidder.to_account_info(),
                    },
                    signer,
                );
                anchor_lang::system_program::transfer(refund_ctx, previous_bid)?;

                emit!(BidRefunded {
                    auction: ctx.accounts.auction.key(),
                    bidder: previous_bidder_key,
                    amount: previous_bid,
                });
            }
        }

        // Transfer new bid to auction account
        let bid_transfer_ctx = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            anchor_lang::system_program::Transfer {
                from: ctx.accounts.bidder.to_account_info(),
                to: ctx.accounts.auction.to_account_info(),
            },
        );
        anchor_lang::system_program::transfer(bid_transfer_ctx, bid_amount)?;

        // Now get mutable reference to update auction state
        let auction = &mut ctx.accounts.auction;
        
        // Update auction state
        auction.highest_bid = bid_amount;
        auction.highest_bidder = Some(ctx.accounts.bidder.key());
        auction.total_bids = auction.total_bids.checked_add(1)
            .ok_or(AuctionError::MathOverflow)?;

        // Extend auction if bid placed in last 10 minutes
        let time_remaining = auction.end_time - clock.unix_timestamp;
        if time_remaining <= 600 { // 10 minutes
            auction.end_time = clock.unix_timestamp + 600; // Extend by 10 minutes
        }

        emit!(BidPlaced {
            auction: auction.key(),
            bidder: ctx.accounts.bidder.key(),
            amount: bid_amount,
            new_end_time: auction.end_time,
        });

        Ok(())
    }

    /// Claim auction (settle) - can be called by winner or seller
    pub fn claim_auction(ctx: Context<ClaimAuction>) -> Result<()> {
        let clock = Clock::get()?;
        
        // Validate auction can be settled (read-only access)
        require!(!ctx.accounts.auction.is_settled, AuctionError::AuctionAlreadySettled);
        require!(!ctx.accounts.auction.is_canceled, AuctionError::AuctionCanceled);
        require!(clock.unix_timestamp >= ctx.accounts.auction.end_time, AuctionError::AuctionNotEnded);
        
        // Store values before mutable access
        let highest_bid = ctx.accounts.auction.highest_bid;
        let reserve_price = ctx.accounts.auction.reserve_price;
        let highest_bidder = ctx.accounts.auction.highest_bidder;
        let auction_bump = ctx.accounts.auction.bump;
        let mint_key = ctx.accounts.auction.mint;
        let seller_key = ctx.accounts.auction.seller;
        let auction_key = ctx.accounts.auction.key();

        // Check if reserve price was met
        if highest_bid < reserve_price {
            // Reserve not met - return NFT to seller
            let auction_seeds = &[
                b"auction",
                mint_key.as_ref(),
                seller_key.as_ref(),
                &[auction_bump],
            ];
            let signer = &[&auction_seeds[..]];

            let nft_transfer_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.auction_token_account.to_account_info(),
                    to: ctx.accounts.seller_token_account.to_account_info(),
                    authority: ctx.accounts.auction.to_account_info(),
                },
                signer,
            );
            token::transfer(nft_transfer_ctx, 1)?;

            // Refund highest bidder if any
            if highest_bid > 0 {
                if let Some(_highest_bidder) = highest_bidder {
                    let refund_ctx = CpiContext::new_with_signer(
                        ctx.accounts.system_program.to_account_info(),
                        anchor_lang::system_program::Transfer {
                            from: ctx.accounts.auction.to_account_info(),
                            to: ctx.accounts.winner.to_account_info(),
                        },
                        signer,
                    );
                    anchor_lang::system_program::transfer(refund_ctx, highest_bid)?;
                }
            }

            // Update auction state
            ctx.accounts.auction.is_settled = true;

            emit!(AuctionSettledNoSale {
                auction: auction_key,
                seller: seller_key,
                highest_bid,
                reserve_price,
            });
        } else {
            // Reserve met - complete the sale
            let sale_price = highest_bid;
            
            // Calculate platform fee
            let platform_fee = ctx.accounts.marketplace.calculate_platform_fee(sale_price)?;
            let seller_proceeds = sale_price.checked_sub(platform_fee)
                .ok_or(AuctionError::MathOverflow)?;

            let auction_seeds = &[
                b"auction",
                mint_key.as_ref(),
                seller_key.as_ref(),
                &[auction_bump],
            ];
            let signer = &[&auction_seeds[..]];

            // Transfer platform fee to treasury
            if platform_fee > 0 {
                let fee_transfer_ctx = CpiContext::new_with_signer(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.auction.to_account_info(),
                        to: ctx.accounts.treasury.to_account_info(),
                    },
                    signer,
                );
                anchor_lang::system_program::transfer(fee_transfer_ctx, platform_fee)?;
            }

            // Transfer proceeds to seller
            let seller_transfer_ctx = CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.auction.to_account_info(),
                    to: ctx.accounts.seller.to_account_info(),
                },
                signer,
            );
            anchor_lang::system_program::transfer(seller_transfer_ctx, seller_proceeds)?;

            // Transfer NFT to winner
            let nft_transfer_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.auction_token_account.to_account_info(),
                    to: ctx.accounts.winner_token_account.to_account_info(),
                    authority: ctx.accounts.auction.to_account_info(),
                },
                signer,
            );
            token::transfer(nft_transfer_ctx, 1)?;

            // Update auction state
            ctx.accounts.auction.is_settled = true;

            emit!(AuctionSettled {
                auction: auction_key,
                seller: seller_key,
                winner: highest_bidder.unwrap(),
                final_price: sale_price,
                platform_fee,
            });
        }

        Ok(())
    }

    /// Cancel auction (only seller, only before first bid)
    pub fn cancel_auction(ctx: Context<CancelAuction>) -> Result<()> {
        // Validate auction can be canceled (read-only access)
        require!(!ctx.accounts.auction.is_settled, AuctionError::AuctionAlreadySettled);
        require!(!ctx.accounts.auction.is_canceled, AuctionError::AuctionAlreadyCanceled);
        require!(ctx.accounts.auction.total_bids == 0, AuctionError::AuctionHasBids);

        // Store values before mutable access
        let auction_bump = ctx.accounts.auction.bump;
        let mint_key = ctx.accounts.auction.mint;
        let seller_key = ctx.accounts.auction.seller;
        let auction_key = ctx.accounts.auction.key();

        // Return NFT to seller
        let auction_seeds = &[
            b"auction",
            mint_key.as_ref(),
            seller_key.as_ref(),
            &[auction_bump],
        ];
        let signer = &[&auction_seeds[..]];

        let nft_transfer_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.auction_token_account.to_account_info(),
                to: ctx.accounts.seller_token_account.to_account_info(),
                authority: ctx.accounts.auction.to_account_info(),
            },
            signer,
        );
        token::transfer(nft_transfer_ctx, 1)?;

        // Update auction state
        ctx.accounts.auction.is_canceled = true;

        emit!(AuctionCanceled {
            auction: auction_key,
            seller: seller_key,
            mint: mint_key,
        });

        Ok(())
    }

    /// Emergency refund for stuck bids (admin only)
    pub fn emergency_refund(ctx: Context<EmergencyRefund>) -> Result<()> {
        // Only marketplace admin can call this
        require!(
            ctx.accounts.marketplace.authority == ctx.accounts.admin.key(),
            AuctionError::Unauthorized
        );

        // Store values before using in transfer
        let auction_bump = ctx.accounts.auction.bump;
        let mint_key = ctx.accounts.auction.mint;
        let seller_key = ctx.accounts.auction.seller;
        let highest_bid = ctx.accounts.auction.highest_bid;
        let auction_key = ctx.accounts.auction.key();

        // Refund the stuck bid
        let auction_seeds = &[
            b"auction",
            mint_key.as_ref(),
            seller_key.as_ref(),
            &[auction_bump],
        ];
        let signer = &[&auction_seeds[..]];

        let refund_ctx = CpiContext::new_with_signer(
            ctx.accounts.system_program.to_account_info(),
            anchor_lang::system_program::Transfer {
                from: ctx.accounts.auction.to_account_info(),
                to: ctx.accounts.refund_recipient.to_account_info(),
            },
            signer,
        );
        anchor_lang::system_program::transfer(refund_ctx, highest_bid)?;

        emit!(EmergencyRefundIssued {
            auction: auction_key,
            recipient: ctx.accounts.refund_recipient.key(),
            amount: highest_bid,
            admin: ctx.accounts.admin.key(),
        });

        Ok(())
    }
}

#[derive(Accounts)]
pub struct CreateAuction<'info> {
    #[account(
        init,
        payer = seller,
        space = 8 + AuctionState::INIT_SPACE,
        seeds = [b"auction", mint.key().as_ref(), seller.key().as_ref()],
        bump
    )]
    pub auction: Account<'info, AuctionState>,
    
    #[account(mut)]
    pub seller: Signer<'info>,
    
    pub mint: Account<'info, Mint>,
    
    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = seller,
        constraint = seller_token_account.amount == 1
    )]
    pub seller_token_account: Account<'info, TokenAccount>,
    
    #[account(
        init,
        payer = seller,
        associated_token::mint = mint,
        associated_token::authority = auction
    )]
    pub auction_token_account: Account<'info, TokenAccount>,
    
    /// CHECK: Metadata account for the NFT - using AccountInfo instead of Account
    pub metadata: AccountInfo<'info>,
    
    pub marketplace: Account<'info, MarketplaceState>,
    
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct PlaceBid<'info> {
    #[account(
        mut,
        seeds = [b"auction", auction.mint.as_ref(), auction.seller.as_ref()],
        bump = auction.bump
    )]
    pub auction: Account<'info, AuctionState>,
    
    #[account(mut)]
    pub bidder: Signer<'info>,
    
    /// CHECK: Previous highest bidder for refunds
    #[account(mut)]
    pub previous_bidder: AccountInfo<'info>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimAuction<'info> {
    #[account(
        mut,
        seeds = [b"auction", auction.mint.as_ref(), auction.seller.as_ref()],
        bump = auction.bump
    )]
    pub auction: Account<'info, AuctionState>,
    
    /// CHECK: Either seller or winner can claim
    #[account(mut)]
    pub claimer: Signer<'info>,
    
    /// CHECK: Seller account for payment
    #[account(
        mut,
        constraint = seller.key() == auction.seller
    )]
    pub seller: AccountInfo<'info>,
    
    /// CHECK: Winner account (highest bidder)
    #[account(mut)]
    pub winner: AccountInfo<'info>,
    
    #[account(
        mut,
        associated_token::mint = auction.mint,
        associated_token::authority = auction
    )]
    pub auction_token_account: Account<'info, TokenAccount>,
    
    #[account(
        mut,
        associated_token::mint = auction.mint,
        associated_token::authority = seller
    )]
    pub seller_token_account: Account<'info, TokenAccount>,
    
    /// NFT mint account
    pub mint: Account<'info, Mint>,

    #[account(
        init_if_needed,
        payer = claimer,
        associated_token::mint = mint,
        associated_token::authority = winner,
        constraint = mint.key() == auction.mint
    )]
    pub winner_token_account: Account<'info, TokenAccount>,
    
    pub marketplace: Account<'info, MarketplaceState>,
    
    /// CHECK: Treasury account from marketplace
    #[account(
        mut,
        constraint = treasury.key() == marketplace.treasury
    )]
    pub treasury: AccountInfo<'info>,
    
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct CancelAuction<'info> {
    #[account(
        mut,
        seeds = [b"auction", auction.mint.as_ref(), auction.seller.as_ref()],
        bump = auction.bump,
        has_one = seller
    )]
    pub auction: Account<'info, AuctionState>,
    
    #[account(mut)]
    pub seller: Signer<'info>,
    
    #[account(
        mut,
        associated_token::mint = auction.mint,
        associated_token::authority = auction
    )]
    pub auction_token_account: Account<'info, TokenAccount>,
    
    #[account(
        mut,
        associated_token::mint = auction.mint,
        associated_token::authority = seller
    )]
    pub seller_token_account: Account<'info, TokenAccount>,
    
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct EmergencyRefund<'info> {
    #[account(
        mut,
        seeds = [b"auction", auction.mint.as_ref(), auction.seller.as_ref()],
        bump = auction.bump
    )]
    pub auction: Account<'info, AuctionState>,
    
    pub admin: Signer<'info>,
    
    /// CHECK: Recipient of the refund
    #[account(mut)]
    pub refund_recipient: AccountInfo<'info>,
    
    pub marketplace: Account<'info, MarketplaceState>,
    pub system_program: Program<'info, System>,
}

#[account]
#[derive(InitSpace)]
pub struct AuctionState {
    pub seller: Pubkey,              // 32
    pub mint: Pubkey,                // 32
    pub start_time: i64,             // 8
    pub end_time: i64,               // 8
    pub reserve_price: u64,          // 8
    pub min_bid_increment: u64,      // 8
    pub highest_bid: u64,            // 8
    pub highest_bidder: Option<Pubkey>, // 1 + 32
    pub total_bids: u64,             // 8
    pub is_settled: bool,            // 1
    pub is_canceled: bool,           // 1
    pub bump: u8,                    // 1
}

impl AuctionState {
    pub const INIT_SPACE: usize = 32 + 32 + 8 + 8 + 8 + 8 + 8 + 1 + 32 + 8 + 1 + 1 + 1; // 148 bytes
}

// Define MarketplaceState here since it's referenced by your auction program
#[account]
#[derive(InitSpace)]
pub struct MarketplaceState {
    pub authority: Pubkey,           // 32
    pub treasury: Pubkey,            // 32
    pub platform_fee_bps: u16,       // 2
    pub is_paused: bool,             // 1
    pub total_volume: u64,           // 8
    pub total_sales: u64,            // 8
    pub bump: u8,                    // 1
}

impl MarketplaceState {
    pub const INIT_SPACE: usize = 32 + 32 + 2 + 1 + 8 + 8 + 1; // 84 bytes
    
    pub fn calculate_platform_fee(&self, sale_price: u64) -> Result<u64> {
        let fee = (sale_price as u128)
            .checked_mul(self.platform_fee_bps as u128)
            .ok_or(AuctionError::MathOverflow)?
            .checked_div(10000)
            .ok_or(AuctionError::MathOverflow)?;
        
        Ok(fee as u64)
    }
}

#[event]
pub struct AuctionCreated {
    pub auction: Pubkey,
    pub seller: Pubkey,
    pub mint: Pubkey,
    pub start_time: i64,
    pub end_time: i64,
    pub reserve_price: u64,
    pub min_bid_increment: u64,
}

#[event]
pub struct BidPlaced {
    pub auction: Pubkey,
    pub bidder: Pubkey,
    pub amount: u64,
    pub new_end_time: i64,
}

#[event]
pub struct BidRefunded {
    pub auction: Pubkey,
    pub bidder: Pubkey,
    pub amount: u64,
}

#[event]
pub struct AuctionSettled {
    pub auction: Pubkey,
    pub seller: Pubkey,
    pub winner: Pubkey,
    pub final_price: u64,
    pub platform_fee: u64,
}

#[event]
pub struct AuctionSettledNoSale {
    pub auction: Pubkey,
    pub seller: Pubkey,
    pub highest_bid: u64,
    pub reserve_price: u64,
}

#[event]
pub struct AuctionCanceled {
    pub auction: Pubkey,
    pub seller: Pubkey,
    pub mint: Pubkey,
}

#[event]
pub struct EmergencyRefundIssued {
    pub auction: Pubkey,
    pub recipient: Pubkey,
    pub amount: u64,
    pub admin: Pubkey,
}

#[error_code]
pub enum AuctionError {
    #[msg("Invalid start time")]
    InvalidStartTime,
    #[msg("Invalid end time")]
    InvalidEndTime,
    #[msg("Invalid reserve price")]
    InvalidReservePrice,
    #[msg("Invalid bid increment")]
    InvalidBidIncrement,
    #[msg("Auction too short (minimum 1 hour)")]
    AuctionTooShort,
    #[msg("Auction too long (maximum 30 days)")]
    AuctionTooLong,
    #[msg("Auction not started")]
    AuctionNotStarted,
    #[msg("Auction has ended")]
    AuctionEnded,
    #[msg("Auction has not ended")]
    AuctionNotEnded,
    #[msg("Bid below reserve price")]
    BidBelowReserve,
    #[msg("Bid too low")]
    BidTooLow,
    #[msg("Auction already settled")]
    AuctionAlreadySettled,
    #[msg("Auction canceled")]
    AuctionCanceled,
    #[msg("Auction already canceled")]
    AuctionAlreadyCanceled,
    #[msg("Cannot cancel auction with bids")]
    AuctionHasBids,
    #[msg("Marketplace is currently paused")]
    MarketplacePaused,
    #[msg("Math overflow occurred")]
    MathOverflow,
    #[msg("Unauthorized access")]
    Unauthorized,
}